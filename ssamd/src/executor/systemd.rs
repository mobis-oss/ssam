// Copyright 2025-2026 Hyundai Mobis Co., Ltd.
// SPDX-License-Identifier: Apache-2.0

use std::str::FromStr as _;
use std::{collections::HashMap, sync::Arc};

use anyhow::Context as _;
use futures_util::{Stream, StreamExt as _};
use libssam::container::ContainerServiceType;
use libssam::ssam_package::ssam_pkg_metadata::PackageMetadata;
use rsactor::{Actor, ActorRef, message_handlers};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use zbus::zvariant::{Array, ObjectPath, OwnedValue, Str, Value};

use systemd_dbus::JobResult;

use crate::configuration;
use crate::executor::systemd::systemd_dbus::manager_messages::{
    ResetFailedUnit, StartTransientUnit, StopUnit,
};
use crate::executor::{CommandArguments, ExecutionResult, ExecutionState, ExecutionStatus};

use super::CommandExecutorBackend;

type JobRemovedSenderMap = HashMap<ObjectPath<'static>, oneshot::Sender<JobResult>>;

mod constants {
    pub(crate) const UNIT_PREFIX: &str = "/org/freedesktop/systemd1/unit";
    pub(crate) const JOB_WAIT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
    pub(crate) const START_TIMEOUT_SEC: u64 = 5;
    pub(crate) const STOP_TIMEOUT_SEC: u64 = 5;
    pub(crate) const USEC_PER_SEC: u64 = 1_000_000;
}

mod systemd_dbus {
    use crate::utils::actor_supervisor::{IgnoreOnFailure, SupervisedActor, spawn_with};
    use anyhow::Context;
    use futures_util::StreamExt;
    use rsactor::{Actor, ActorRef, ActorWeak, message_handlers};
    use strum::Display;
    use tokio::sync::oneshot;

    use zbus_systemd::systemd1::{JobRemovedStream, ManagerProxy};

    use crate::executor::systemd::constants;

    use super::JobRemovedSenderMap;

    static DBUS_SYSTEM_CONNECTION: tokio::sync::OnceCell<zbus::Connection> =
        tokio::sync::OnceCell::const_new();

    #[derive(derive_more::Deref)]
    pub(crate) struct SharedSystemDConnection(zbus::Connection);

    impl SharedSystemDConnection {
        pub(crate) async fn new() -> anyhow::Result<Self> {
            let connection = DBUS_SYSTEM_CONNECTION
                .get_or_try_init(|| async {
                    let connection = zbus::Connection::system().await?;
                    Ok::<_, anyhow::Error>(connection)
                })
                .await
                .context("Failed to get dbus connection")?;

            Ok(Self(connection.clone()))
        }
    }

    mod watcher_messages {
        use zbus::zvariant::ObjectPath;

        pub(crate) struct WatchSignal {
            pub(crate) job_path: ObjectPath<'static>,
        }

        pub(crate) struct CancelWatchSignal {
            pub(crate) job_path: ObjectPath<'static>,
        }
    }

    #[derive(Debug)]
    struct JobRemovedSignalWatcherActor {
        stream: JobRemovedStream,
        senders: JobRemovedSenderMap,
    }

    impl rsactor::Actor for JobRemovedSignalWatcherActor {
        type Args = Self;
        type Error = anyhow::Error;

        async fn on_start(args: Self::Args, _actor_ref: &ActorRef<Self>) -> anyhow::Result<Self> {
            log::debug!("SystemDSignalWatcher started");
            Ok(args)
        }

        async fn on_run(&mut self, _actor_ref: &ActorWeak<Self>) -> anyhow::Result<bool> {
            log::debug!("JobRemovedSignalWatcher is running");
            if let Some(signal) = self.stream.next().await {
                let args = match signal.args() {
                    Ok(args) => args,
                    Err(e) => {
                        log::warn!("Got unexpected signal. Report to zbus: {e}");
                        return Ok(true);
                    }
                };

                let result = match args.result().as_str() {
                    "done" => JobResult::Success,
                    "canceled" => JobResult::Canceled,
                    res => JobResult::Failed(res.to_owned()),
                };
                let job = args.job().as_ref().to_owned();
                let unit = args.unit();

                let sender = self.senders.remove(&job);
                if let Some(sender) = sender {
                    log::debug!(
                        "Send job result to watcher for {job}. unit: {unit}, result: {result}",
                    );
                    let _ = sender.send(result);
                }
                Ok(true)
            } else {
                anyhow::bail!("JobRemovedStream ended unexpectedly");
            }
        }
    }

    #[message_handlers]
    impl JobRemovedSignalWatcherActor {
        fn new(stream: JobRemovedStream) -> Self {
            Self {
                stream,
                senders: JobRemovedSenderMap::new(),
            }
        }

        #[handler]
        // rsactor #[handler] requires async fn signature even without await
        #[allow(clippy::unused_async)]
        async fn handle_watch_signal(
            &mut self,
            msg: watcher_messages::WatchSignal,
            _actor_ref: &ActorRef<Self>,
        ) -> oneshot::Receiver<JobResult> {
            log::debug!("WatchSignal, job_path: {}", msg.job_path);
            let (sender, receiver) = oneshot::channel();
            self.senders.insert(msg.job_path.to_owned(), sender);
            receiver
        }

        #[handler]
        // rsactor #[handler] requires async fn signature even without await
        #[allow(clippy::unused_async)]
        async fn handle_cancel_watch_signal(
            &mut self,
            msg: watcher_messages::CancelWatchSignal,
            _actor_ref: &ActorRef<Self>,
        ) -> Option<oneshot::Sender<JobResult>> {
            log::debug!("CancelWatchSignal, job_path: {}", msg.job_path);
            self.senders.remove(&msg.job_path)
        }
    }

    impl SupervisedActor for JobRemovedSignalWatcherActor {
        type FailurePolicy = IgnoreOnFailure;
    }

    #[derive(Debug, Display)]
    pub(crate) enum JobResult {
        Success,
        Canceled,
        Failed(String),
        Timeout,
    }

    pub(crate) mod manager_messages {
        pub(crate) struct ResetFailedUnit {
            pub(crate) unit_name: String,
        }

        pub(crate) struct StartTransientUnit {
            pub(crate) unit_name: String,
            pub(crate) properties: Vec<(String, zbus::zvariant::OwnedValue)>,
        }

        pub(crate) struct StopUnit {
            pub(crate) unit_name: String,
        }
    }

    #[derive(Debug, Actor)]
    pub(crate) struct SystemdManagerActor {
        manager_proxy: ManagerProxy<'static>,
        job_removed_watcher_actor: ActorRef<JobRemovedSignalWatcherActor>,
    }

    impl SupervisedActor for SystemdManagerActor {
        type FailurePolicy = IgnoreOnFailure;
    }

    #[message_handlers]
    impl SystemdManagerActor {
        async fn new(connection: &zbus::Connection) -> anyhow::Result<Self> {
            let manager_proxy = ManagerProxy::new(connection)
                .await
                .context("Failed to create ManagerProxy")?;

            // Subscribe() enables most systemd D-Bus signals to be sent out. Signals are only
            // delivered if at least one client has invoked Subscribe(). At ssamd startup, there
            // may be no other clients, so we call subscribe here to ensure signal delivery
            // for safety.
            manager_proxy
                .subscribe()
                .await
                .context("Failed to subscribe to systemd")?;
            let stream = manager_proxy
                .receive_job_removed()
                .await
                .context("Failed to create JobRemovedStream")?;
            let job_removed_watcher = JobRemovedSignalWatcherActor::new(stream);
            let job_removed_watcher =
                spawn_with::<JobRemovedSignalWatcherActor>(job_removed_watcher);

            Ok(Self {
                manager_proxy,
                job_removed_watcher_actor: job_removed_watcher,
            })
        }

        #[handler]
        async fn handle_reset_failed_unit(
            &mut self,
            msg: manager_messages::ResetFailedUnit,
            _actor_ref: &ActorRef<Self>,
        ) -> anyhow::Result<()> {
            self.manager_proxy
                .reset_failed_unit(msg.unit_name)
                .await
                .context("Failed to reset failed unit")
        }

        #[handler]
        async fn handle_start_transient_unit(
            &mut self,
            msg: manager_messages::StartTransientUnit,
            _actor_ref: &ActorRef<Self>,
        ) -> anyhow::Result<JobResult> {
            let properties = msg.properties;
            let job_path = self
                .manager_proxy
                .start_transient_unit(msg.unit_name, "replace".to_owned(), properties, vec![])
                .await
                .context("Failed to start transient unit")?;

            let job_path = job_path.into_inner();
            log::debug!("Ask to watch Job path: {job_path}");
            let recv = self
                .job_removed_watcher_actor
                .ask(watcher_messages::WatchSignal {
                    job_path: job_path.to_owned(),
                })
                .await
                .context("JobRemoveWatcher has been stopped or not initialized")?;

            let result = tokio::time::timeout(constants::JOB_WAIT_TIMEOUT, recv).await;
            if let Ok(result) = result {
                result.context("JobRemovedSignalWatcher has been stopped or not initialized")
            } else {
                log::warn!("Start has timed out for job: {job_path}");
                self.job_removed_watcher_actor
                    .ask(watcher_messages::CancelWatchSignal {
                        job_path: job_path.to_owned(),
                    })
                    .await
                    .context("JobRemoveWatcher has been stopped or not initialized")?;
                Ok(JobResult::Timeout)
            }
        }

        #[handler]
        async fn handle_stop_unit(
            &mut self,
            msg: manager_messages::StopUnit,
            _actor_ref: &ActorRef<Self>,
        ) -> anyhow::Result<JobResult> {
            let job_path = self
                .manager_proxy
                .stop_unit(msg.unit_name, "replace".to_owned())
                .await
                .context("Failed to stop unit")?
                .into_inner();

            let recv = self
                .job_removed_watcher_actor
                .ask(watcher_messages::WatchSignal {
                    job_path: job_path.to_owned(),
                })
                .await
                .context("JobRemoveWatcher has been stopped or not initialized")?;

            let result = tokio::time::timeout(constants::JOB_WAIT_TIMEOUT, recv).await;
            if let Ok(result) = result {
                result.context("JobRemovedSignalWatcher has been stopped or not initialized")
            } else {
                log::warn!("Stop has timed out for job: {job_path}");
                self.job_removed_watcher_actor
                    .ask(watcher_messages::CancelWatchSignal {
                        job_path: job_path.to_owned(),
                    })
                    .await
                    .context("JobRemoveWatcher has been stopped or not initialized")?;
                Ok(JobResult::Timeout)
            }
        }
    }

    static SYSTEMD_MANAGER_INTERFACE: tokio::sync::OnceCell<ActorRef<SystemdManagerActor>> =
        tokio::sync::OnceCell::const_new();

    #[derive(Debug, derive_more::Deref)]
    pub(crate) struct SystemdManager(ActorRef<SystemdManagerActor>);

    impl SystemdManager {
        pub(crate) async fn new() -> anyhow::Result<Self> {
            let manager_ref = SYSTEMD_MANAGER_INTERFACE
                .get_or_try_init(|| async {
                    let connection = SharedSystemDConnection::new().await?;
                    let manager_interface = SystemdManagerActor::new(&connection).await?;
                    let actor_ref = spawn_with::<SystemdManagerActor>(manager_interface);
                    Ok::<_, anyhow::Error>(actor_ref)
                })
                .await?;
            Ok(Self(manager_ref.clone()))
        }
    }
}

mod utils {
    pub(crate) fn slice_str_from_cgroups_path(cgroups_path: &str) -> anyhow::Result<&str> {
        if cgroups_path.is_empty() {
            return Err(anyhow::anyhow!(
                "Cannot get slice name from empty cgroups path"
            ));
        }
        // This is a string suffix check, not a file extension comparison.
        // The cgroups path is a systemd slice name, not a filesystem path with an extension.
        #[allow(clippy::case_sensitive_file_extension_comparisons)]
        if !cgroups_path.ends_with(".slice") {
            return Err(anyhow::anyhow!("cgroups path does not end with .slice"));
        }
        let slice_str = cgroups_path
            .rsplit_once('/')
            .map_or(cgroups_path, |(_, s)| s);
        Ok(slice_str)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct FailureInfo {
    result: String,
    exitcode: i32,
}

// To use strum_macros::EnumString, implement default for FailureInfo
impl Default for FailureInfo {
    fn default() -> Self {
        Self {
            result: "Unknown".to_owned(),
            exitcode: -1,
        }
    }
}

impl std::fmt::Display for FailureInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Result: {result}, Exitcode: {exitcode}",
            result = self.result,
            exitcode = self.exitcode
        )
    }
}

struct UnitActiveStateChangedMsg {
    state: UnitActiveState,
}

#[derive(Debug)]
struct ActiveStateConverterActor {
    active_state_sender: mpsc::Sender<ExecutionStatus>,
}

impl Actor for ActiveStateConverterActor {
    type Args = mpsc::Sender<ExecutionStatus>;
    type Error = anyhow::Error;

    async fn on_start(args: Self::Args, _actor_ref: &ActorRef<Self>) -> anyhow::Result<Self> {
        Ok(Self {
            active_state_sender: args,
        })
    }
}

#[message_handlers]
impl ActiveStateConverterActor {
    #[handler]
    async fn handle_unit_active_state_changed(
        &mut self,
        msg: UnitActiveStateChangedMsg,
        _actor_ref: &ActorRef<Self>,
    ) -> anyhow::Result<()> {
        let state: ExecutionStatus = msg.state.into();
        self.active_state_sender
            .send(state)
            .await
            .context("Failed to send active state to executor")?;
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ServiceInfo {
    pub(crate) description: String,
    pub(crate) service_type: ContainerServiceType,
    pub(crate) remain_after_exit: Option<bool>,
    pub(crate) bus_name: Option<String>,
}

impl ServiceInfo {
    pub(crate) fn new_from_metadata(metadata: &PackageMetadata) -> anyhow::Result<Self> {
        let package_name = metadata.get_package_name();
        let package_conf = &metadata.package;
        let description = package_conf.description.clone();
        let service_config = &metadata.service;
        let service_type = ContainerServiceType::from_str(&service_config.service_type)
            .with_context(|| format!("Invalid service type in package {package_name}"))?;
        let remain_after_exit = service_config.remain_after_exit;
        let bus_name = service_config.bus_name.clone();
        Ok(Self {
            description,
            service_type,
            remain_after_exit,
            bus_name,
        })
    }
}

#[derive(Debug)]
pub(crate) struct TransientUnitExecutor<'u> {
    unit_name: String,
    cmd_args: Arc<dyn CommandArguments>,
    service_info: ServiceInfo,
    active_state_handler: ActiveStateHandler<'u>,
    systemd_interface: systemd_dbus::SystemdManager,
}

const DEF_CGROUPS_PATH: &str = "system.slice";
const BLOCKING_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(8);

pub(crate) fn cgroups_path() -> &'static str {
    let packages_cgroup = configuration::packages_cgroup();
    if packages_cgroup.is_empty() {
        DEF_CGROUPS_PATH
    } else {
        packages_cgroup
    }
}

impl TransientUnitExecutor<'_> {
    pub(crate) async fn new(
        package_name: String,
        cmd_args: Arc<dyn CommandArguments>,
        service_info: ServiceInfo,
        active_state_sender: mpsc::Sender<ExecutionStatus>,
    ) -> anyhow::Result<Self> {
        let systemd_interface = systemd_dbus::SystemdManager::new()
            .await
            .context("Failed to create systemd interface")?;
        let connection = systemd_dbus::SharedSystemDConnection::new().await?;
        let (state_converter, _) =
            rsactor::spawn::<ActiveStateConverterActor>(active_state_sender.clone());
        let unit_name = format!("{package_name}.service");
        let active_state_handler =
            ActiveStateHandler::new(&connection, &unit_name, state_converter)
                .await
                .context("Failed to create ActiveStateHandler")?;
        Ok(Self {
            unit_name,
            cmd_args,
            service_info,
            active_state_handler,
            systemd_interface,
        })
    }

    fn gen_unit_properties(&self) -> anyhow::Result<Vec<(String, OwnedValue)>> {
        let start_cmd = self.cmd_args.get_start_args()?;
        let execstart: OwnedValue = Array::from(vec![(start_cmd[0].clone(), start_cmd, false)])
            .try_into()
            .context(format!(
                "Failed to convert execstart command for unit {}",
                self.unit_name
            ))?;

        let delete_cmd = self.cmd_args.get_stop_args()?;
        let exec_stop_post: OwnedValue =
            Array::from(vec![(delete_cmd[0].clone(), delete_cmd, false)])
                .try_into()
                .context(format!(
                    "Failed to convert exec_stop_post command for unit {}",
                    self.unit_name
                ))?;

        let exitcodes = vec![143];
        let signals: Vec<i32> = Vec::new();
        let success_exit_codes: Value = (exitcodes, signals).into();
        let success_exit_codes: OwnedValue = success_exit_codes.try_to_owned().context(format!(
            "Failed to convert success_exit_codes for unit {}",
            self.unit_name
        ))?;

        let start_timeout_usec: u64 = constants::START_TIMEOUT_SEC * constants::USEC_PER_SEC;
        let stop_timeout_usec: u64 = constants::STOP_TIMEOUT_SEC * constants::USEC_PER_SEC;

        let description = self.service_info.description.as_str();

        let mut properties = vec![
            ("Description".to_owned(), Str::from(description).into()),
            ("TimeoutStartUSec".to_owned(), start_timeout_usec.into()),
            ("TimeoutStopUSec".to_owned(), stop_timeout_usec.into()),
            ("Restart".to_owned(), Str::from("on-failure").into()),
            ("ExecStart".to_owned(), execstart),
            ("ExecStopPost".to_owned(), exec_stop_post),
        ];

        let cgroups_path = cgroups_path();
        match utils::slice_str_from_cgroups_path(cgroups_path) {
            Ok(slice_str) => {
                properties.push(("Slice".to_owned(), Str::from(slice_str).into()));
            }
            Err(e) => {
                log::warn!("Failed to get slice name from cgroups path {cgroups_path}: {e}");
            }
        }

        let type_str: &str = self.service_info.service_type.into();
        properties.push(("Type".to_owned(), Str::from(type_str).into()));
        let remain_after_exit = self.service_info.remain_after_exit.unwrap_or_default();
        properties.push(("RemainAfterExit".to_owned(), remain_after_exit.into()));

        let service_type = ContainerServiceType::from_str(type_str).context(format!(
            "Invalid Service type {type_str}; cannot be launched"
        ))?;

        match service_type {
            ContainerServiceType::Notify => {
                // As long as crun send sd_notify from forked process, NotifyAccess should be 'all'
                properties.push(("NotifyAccess".to_owned(), Str::from("all").into()));
                properties.push(("SuccessExitStatus".to_owned(), success_exit_codes));
            }
            ContainerServiceType::DBus => {
                properties.push(("Requires".to_owned(), Str::from("dbus.service").into()));
                if let Some(bus_name) = self.service_info.bus_name.as_ref() {
                    properties.push(("BusName".to_owned(), Str::from(bus_name).into()));
                }
            }
            ContainerServiceType::Oneshot
            | ContainerServiceType::Simple
            | ContainerServiceType::Exec
            | ContainerServiceType::Forking
            | ContainerServiceType::Idle => {}
        }

        Ok(properties.into_iter().collect())
    }

    fn unit_name(&self) -> &str {
        &self.unit_name
    }

    async fn get_active_state(&self) -> anyhow::Result<UnitActiveState> {
        self.active_state_handler.get_active_state().await
    }

    async fn reset_failed(&self) -> anyhow::Result<()> {
        self.systemd_interface
            .ask(ResetFailedUnit {
                unit_name: self.unit_name().to_owned(),
            })
            .await
            .context("SystemdManagerActor has died?")?
    }

    async fn handle_job_result(
        &self,
        result: anyhow::Result<JobResult>,
    ) -> anyhow::Result<ExecutionResult> {
        log::trace!("handle_job_result: {} {:?}", self.unit_name(), result);
        let inner_state = self.get_active_state().await.context(format!(
            "Cannot get inner state of unit: {}",
            self.unit_name()
        ))?;
        let status: ExecutionStatus = inner_state.into();
        result.map(|job_result| map_job_result(self.unit_name(), job_result, status))
    }
}

// JobResult::Failed/Timeout but unit is Active means the job dispatcher
// reported failure, but the unit itself is in a known active state. Reflect
// the actual runtime state rather than treating it as an error.
// ExecutionResult::Failure is reserved for cases where the unit did not
// reach a usable active state (e.g., dependency failures leaving the unit
// Inactive, or D-Bus communication errors in the caller).
fn map_job_result(
    unit_name: &str,
    job_result: JobResult,
    status: ExecutionStatus,
) -> ExecutionResult {
    match job_result {
        JobResult::Success => {
            log::debug!("{unit_name}: Operation finished successfully.");
            ExecutionResult::Success(status)
        }
        JobResult::Canceled => {
            log::warn!("{unit_name}: Operation has been canceled");
            // Always propagate as Canceled regardless of Active/Inactive —
            // cancellation is an explicit request, not a failure.
            ExecutionResult::Canceled(status)
        }
        JobResult::Failed(reason) => {
            log::warn!("{unit_name} Operation has failed: {reason}");
            // The reason string is intentionally not propagated into
            // ExecutionResult::Success — the actual runtime state (e.g.,
            // UnitActiveState::Failed with exit code) is already captured
            // inside ExecutionStatus::Active and survives into
            // PackageStatus::Running("failed").
            match &status {
                ExecutionStatus::Active(_) => ExecutionResult::Success(status),
                ExecutionStatus::Inactive(_) => ExecutionResult::Failure(reason),
            }
        }
        JobResult::Timeout => {
            log::warn!("{unit_name} Operation has timed out");
            match &status {
                ExecutionStatus::Active(_) => ExecutionResult::Success(status),
                ExecutionStatus::Inactive(_) => ExecutionResult::Timeout,
            }
        }
    }
}

#[async_trait::async_trait]
impl CommandExecutorBackend for TransientUnitExecutor<'_> {
    async fn start(&self) -> anyhow::Result<ExecutionResult> {
        log::debug!("SystemdTransientUnit::start for {}", self.unit_name);
        let unit_name = self.unit_name();
        let active_state = self
            .active_state_handler
            .get_active_state()
            .await
            .context(format!("Failed to get active state for unit {unit_name}"))?;

        // Idempotent start: any unit already loaded under this name — Active or
        // mid-transition (Activating/Reloading/Deactivating) — is a live resource
        // to reuse; reissuing StartTransientUnit errors with "already loaded".
        // Only a gone unit (Inactive) gets a fresh start; a Failed unit is cleared
        // first so it can be recreated. The monitor wired in `new()` reports the
        // eventual settled state.
        match active_state {
            UnitActiveState::Inactive => {}
            UnitActiveState::Failed(_) => {
                log::debug!("Reset failed state for unit {unit_name}");
                self.reset_failed().await?;
            }
            other => {
                log::debug!("Unit {unit_name} already loaded; skipping StartTransientUnit");
                return Ok(ExecutionResult::Success(other.into()));
            }
        }
        let unit_properties = self
            .gen_unit_properties()
            .with_context(|| format!("Failed to generate properties for unit {unit_name}"))?;
        let job_result = self
            .systemd_interface
            .ask(StartTransientUnit {
                unit_name: self.unit_name().to_owned(),
                properties: unit_properties,
            })
            .await
            .context("Check whether SystemdManagerActor has died.")?;

        self.handle_job_result(job_result).await
    }

    async fn stop(&self) -> anyhow::Result<ExecutionResult> {
        let unit_name = self.unit_name();
        log::debug!("SystemdTransientUnit::stop for {unit_name}");

        let active_state = self
            .active_state_handler
            .get_active_state()
            .await
            .context(format!("Failed to get active state for unit {unit_name}"))?;

        if let UnitActiveState::Failed(_) = active_state {
            // Failed state guarantees the process has already terminated.
            // StopUnit has no effect on failed transient units, so clear the
            // failure record instead; the transient unit is then removed by
            // systemd automatically (no on-disk unit file to keep it around).
            log::debug!("Reset failed state for unit {unit_name} before stop");
            self.reset_failed().await?;
            return Ok(ExecutionResult::Success(UnitActiveState::Inactive.into()));
        }

        let job_result = self
            .systemd_interface
            .ask(StopUnit {
                unit_name: unit_name.to_owned(),
            })
            .await
            .context("Check whether SystemdManagerActor has died.")?;
        self.handle_job_result(job_result).await
    }

    async fn teardown(&self) -> anyhow::Result<()> {
        // Wait for the stop operation to complete or timeout
        let _ = tokio::time::timeout(BLOCKING_TIMEOUT, self.stop()).await;
        self.reset_failed().await
    }
}

#[derive(Clone, Debug, PartialEq, strum_macros::EnumString, strum::Display)]
#[strum(serialize_all = "kebab-case")]
pub(crate) enum UnitActiveState {
    Activating,
    Active,
    Reloading,
    Deactivating,
    Inactive,
    Failed(FailureInfo),
}

impl ExecutionState for UnitActiveState {}

impl From<UnitActiveState> for ExecutionStatus {
    fn from(state: UnitActiveState) -> Self {
        let state = Arc::new(state);
        match state.as_ref() {
            UnitActiveState::Activating
            | UnitActiveState::Active
            | UnitActiveState::Reloading
            | UnitActiveState::Deactivating
            | UnitActiveState::Failed(_) => ExecutionStatus::Active(state),
            UnitActiveState::Inactive => ExecutionStatus::Inactive(state),
        }
    }
}

trait FailureInfoSource {
    async fn failure_info(&self) -> anyhow::Result<FailureInfo>;
}

#[derive(derive_more::Deref)]
struct SystemdServiceProxy<'a>(&'a zbus_systemd::systemd1::ServiceProxy<'a>);

impl FailureInfoSource for SystemdServiceProxy<'_> {
    async fn failure_info(&self) -> anyhow::Result<FailureInfo> {
        let result = self.result().await?;
        let exitcode = self.exec_main_code().await?;
        Ok(FailureInfo { result, exitcode })
    }
}

async fn active_state_from_str(
    state: &str,
    source: &impl FailureInfoSource,
) -> anyhow::Result<UnitActiveState> {
    let state = if state == "failed" {
        let failure_info = source.failure_info().await?;
        UnitActiveState::Failed(failure_info)
    } else {
        UnitActiveState::from_str(state)
            .with_context(|| format!("Cannot convert activestate {state} to ActiveState"))?
    };
    Ok(state)
}

async fn run_active_state_monitor(
    stream: impl Stream<Item = anyhow::Result<String>>,
    failure_source: &impl FailureInfoSource,
    canceler: CancellationToken,
    state_converter: ActorRef<ActiveStateConverterActor>,
    unit_name: &str,
    initial_state: UnitActiveState,
) -> anyhow::Result<()> {
    let mut stream = std::pin::pin!(stream);
    // systemd is the source of truth: emit the unit's real live state once so a
    // recovered (adopted) container is reflected immediately, then forward every
    // change verbatim. The transition manager's same-state no-op absorbs any
    // redundant repeat, so no local dedup cache is kept.
    state_converter
        .tell(UnitActiveStateChangedMsg {
            state: initial_state,
        })
        .await?;
    loop {
        tokio::select! {
            () = canceler.cancelled() => {
                log::debug!("ActiveStateHandler task has been canceled");
                break;
            },
            item = stream.next() => {
                let Some(result) = item else {
                    anyhow::bail!(
                        "D-Bus ActiveState property stream closed unexpectedly for {unit_name}"
                    );
                };
                let state = active_state_from_str(&result?, failure_source)
                    .await
                    .with_context(|| format!("Cannot convert ActiveState for unit {unit_name}"))?;
                state_converter.tell(UnitActiveStateChangedMsg { state }).await?;
            }
        }
    }
    Ok(())
}

#[derive(Debug)]
struct ActiveStateHandler<'a> {
    unit_proxy: zbus_systemd::systemd1::UnitProxy<'a>,
    service_proxy: zbus_systemd::systemd1::ServiceProxy<'a>,
    cancel_token: CancellationToken,
}

impl ActiveStateHandler<'_> {
    async fn new(
        connection: &zbus::Connection,
        unit_name: &str,
        state_converter: ActorRef<ActiveStateConverterActor>,
    ) -> anyhow::Result<Self> {
        let prefix = ObjectPath::try_from(constants::UNIT_PREFIX)?;
        let unit_path = zbus_systemd::bus_path_encode(&prefix, unit_name);
        let unit_proxy = zbus_systemd::systemd1::UnitProxy::new(connection, unit_path.clone())
            .await
            .context(format!("Failed to create unit proxy for {unit_name}."))?;

        let service_proxy =
            zbus_systemd::systemd1::ServiceProxy::new(connection, unit_path.clone())
                .await
                .context(format!("Failed to create service proxy for {unit_name}."))?;

        let service_proxy_on_recv = service_proxy.clone();
        let unit_name = unit_name.to_owned();
        let stream = unit_proxy.receive_active_state_changed().await;
        let cancel_token = CancellationToken::new();
        let canceler = cancel_token.clone();

        // Read the live state once before spawning so the monitor can emit it as
        // its first event, reflecting an already-active (adopted) unit. An
        // unreadable state (unit not yet loaded in the pre-start path) is Inactive.
        let initial_state = match unit_proxy.active_state().await {
            Ok(state) => {
                active_state_from_str(state.as_str(), &SystemdServiceProxy(&service_proxy))
                    .await
                    .unwrap_or_else(|e| {
                        log::warn!(
                            "unit {unit_name}: unrecognized initial systemd active state, \
                             defaulting to Inactive: {e:#}"
                        );
                        UnitActiveState::Inactive
                    })
            }
            Err(_) => UnitActiveState::Inactive,
        };

        tokio::spawn(async move {
            let state_stream = stream.then(|msg| async move {
                msg.get()
                    .await
                    .context("Failed to get active state property")
            });
            let source = SystemdServiceProxy(&service_proxy_on_recv);
            if let Err(e) = run_active_state_monitor(
                state_stream,
                &source,
                canceler,
                state_converter,
                &unit_name,
                initial_state,
            )
            .await
            {
                log::error!("ActiveState monitor for {unit_name} failed: {e:#}");
            }
        });
        Ok(Self {
            unit_proxy,
            service_proxy,
            cancel_token,
        })
    }

    async fn get_active_state(&self) -> anyhow::Result<UnitActiveState> {
        let state = self.unit_proxy.active_state().await?;
        let source = SystemdServiceProxy(&self.service_proxy);
        active_state_from_str(state.as_str(), &source).await
    }
}

impl Drop for ActiveStateHandler<'_> {
    fn drop(&mut self) {
        log::debug!("Drop ActiveStateHandler");
        self.cancel_token.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsactor::spawn;
    use tokio::sync::mpsc;

    struct MockServiceProxy;

    impl FailureInfoSource for MockServiceProxy {
        async fn failure_info(&self) -> anyhow::Result<FailureInfo> {
            Ok(FailureInfo {
                result: "exit-code".to_string(),
                exitcode: 1,
            })
        }
    }

    #[tokio::test]
    async fn test_active_state_converter_handle_unit_active_state_changed() {
        // Arrange
        let (tx, mut rx) = mpsc::channel::<ExecutionStatus>(10);
        let (converter, _handle) = spawn::<ActiveStateConverterActor>(tx);

        // Test various UnitActiveState transitions
        let test_cases = vec![
            (UnitActiveState::Activating, "activating"),
            (UnitActiveState::Active, "active"),
            (UnitActiveState::Reloading, "reloading"),
            (UnitActiveState::Deactivating, "deactivating"),
            (UnitActiveState::Inactive, "inactive"),
            (
                UnitActiveState::Failed(FailureInfo {
                    result: "exit-code".to_string(),
                    exitcode: 1,
                }),
                "failed",
            ),
        ];

        for (unit_state, expected_display) in test_cases {
            // Act
            let result = converter
                .ask(UnitActiveStateChangedMsg {
                    state: unit_state.clone(),
                })
                .await;

            // Assert
            assert!(
                result.is_ok(),
                "Failed to handle state change for {unit_state:?}"
            );

            // Verify the state was sent through the channel
            let received_state = rx.recv().await.expect("Should receive execution state");

            // Verify the correct variant and display string
            let display_state = match &received_state {
                ExecutionStatus::Active(state) | ExecutionStatus::Inactive(state) => {
                    format!("{state}")
                }
            };
            assert_eq!(display_state, expected_display);
        }
    }

    #[tokio::test]
    async fn test_active_state_converter_channel_closed() {
        // Arrange
        let (tx, rx) = mpsc::channel::<ExecutionStatus>(1);
        let (converter, _handle) = spawn::<ActiveStateConverterActor>(tx);

        // Drop the receiver to close the channel
        drop(rx);

        // Act
        let result = converter
            .ask(UnitActiveStateChangedMsg {
                state: UnitActiveState::Active,
            })
            .await
            .unwrap();

        // Assert
        assert!(result.is_err(), "Should fail when channel is closed");
        let error = result.unwrap_err();
        assert!(
            error
                .to_string()
                .contains("Failed to send active state to executor")
        );
    }

    #[test]
    fn test_unit_active_state_to_execution_state_conversion() {
        // Test Active states - these should convert to ExecutionState::Active
        let active_states = vec![
            UnitActiveState::Activating,
            UnitActiveState::Active,
            UnitActiveState::Reloading,
            UnitActiveState::Deactivating,
            UnitActiveState::Failed(FailureInfo::default()),
        ];

        for state in active_states {
            let execution_state: ExecutionStatus = state.clone().into();
            match execution_state {
                ExecutionStatus::Active(_) => {
                    // Success - this is expected
                }
                ExecutionStatus::Inactive(_) => {
                    panic!("Expected Active state for {state:?}, got Inactive");
                }
            }
        }

        // Test Inactive state - this should convert to ExecutionState::Inactive
        let inactive_state = UnitActiveState::Inactive;
        let execution_state: ExecutionStatus = inactive_state.clone().into();
        match execution_state {
            ExecutionStatus::Inactive(_) => {
                // Success - this is expected
            }
            ExecutionStatus::Active(_) => {
                panic!("Expected Inactive state for Inactive, got Active");
            }
        }
    }

    #[tokio::test]
    async fn test_active_state_converter_multiple_state_changes() {
        // Arrange
        let (tx, mut rx) = mpsc::channel::<ExecutionStatus>(10);
        let (converter, _handle) = spawn::<ActiveStateConverterActor>(tx);

        // Test a sequence of state changes
        let state_sequence = vec![
            UnitActiveState::Activating,
            UnitActiveState::Active,
            UnitActiveState::Deactivating,
            UnitActiveState::Inactive,
        ];

        // Act & Assert
        for state in state_sequence {
            let result = converter
                .ask(UnitActiveStateChangedMsg {
                    state: state.clone(),
                })
                .await;

            assert!(
                result.is_ok(),
                "Failed to handle state change for {state:?}"
            );

            let received_state = rx.recv().await.expect("Should receive execution state");

            // Verify the state conversion is correct
            match state {
                UnitActiveState::Inactive => {
                    assert!(matches!(received_state, ExecutionStatus::Inactive(_)));
                }
                _ => {
                    assert!(matches!(received_state, ExecutionStatus::Active(_)));
                }
            }
        }
    }

    fn active_status(state: UnitActiveState) -> ExecutionStatus {
        state.into()
    }

    fn inactive_status() -> ExecutionStatus {
        UnitActiveState::Inactive.into()
    }

    #[test]
    fn test_map_job_result_success_with_active_status() {
        let status = active_status(UnitActiveState::Active);
        let result = map_job_result("test.service", JobResult::Success, status);
        assert!(matches!(result, ExecutionResult::Success(_)));
    }

    #[test]
    fn test_map_job_result_canceled_preserves_status() {
        let status = active_status(UnitActiveState::Active);
        let result = map_job_result("test.service", JobResult::Canceled, status);
        assert!(matches!(result, ExecutionResult::Canceled(_)));
    }

    #[test]
    fn test_map_job_result_failed_with_active_yields_success() {
        let status = active_status(UnitActiveState::Failed(FailureInfo {
            result: "exit-code".to_string(),
            exitcode: 1,
        }));
        let result = map_job_result(
            "test.service",
            JobResult::Failed("exit-code".into()),
            status,
        );
        assert!(
            matches!(result, ExecutionResult::Success(ExecutionStatus::Active(s)) if format!("{s}") == "failed")
        );
    }

    #[test]
    fn test_map_job_result_failed_with_inactive_yields_failure() {
        let status = inactive_status();
        let result = map_job_result(
            "test.service",
            JobResult::Failed("dependency".into()),
            status,
        );
        assert!(matches!(&result, ExecutionResult::Failure(reason) if reason == "dependency"));
    }

    #[test]
    fn test_map_job_result_timeout_with_active_yields_success() {
        let status = active_status(UnitActiveState::Active);
        let result = map_job_result("test.service", JobResult::Timeout, status);
        assert!(matches!(result, ExecutionResult::Success(_)));
    }

    #[test]
    fn test_map_job_result_timeout_with_inactive_yields_timeout() {
        let status = inactive_status();
        let result = map_job_result("test.service", JobResult::Timeout, status);
        assert!(matches!(result, ExecutionResult::Timeout));
    }

    #[tokio::test]
    async fn test_run_active_state_monitor_exits_on_stream_closure() {
        use futures_util::stream;
        use std::time::Duration;

        let (tx, _rx) = mpsc::channel::<ExecutionStatus>(10);
        let (converter, _handle) = spawn::<ActiveStateConverterActor>(tx);
        let cancel_token = CancellationToken::new();

        let state_stream = stream::empty::<anyhow::Result<String>>();

        let result = tokio::time::timeout(
            Duration::from_secs(1),
            run_active_state_monitor(
                state_stream,
                &MockServiceProxy,
                cancel_token,
                converter,
                "test.service",
                UnitActiveState::Inactive,
            ),
        )
        .await;

        assert!(result.is_ok(), "Should not hang when stream closes");
        assert!(
            result.unwrap().is_err(),
            "Should return Err on unexpected stream closure"
        );
    }

    #[tokio::test]
    async fn test_run_active_state_monitor_emits_initial_then_forwards_all() {
        use futures_util::stream;

        let (tx, mut rx) = mpsc::channel::<ExecutionStatus>(10);
        let (converter, _handle) = spawn::<ActiveStateConverterActor>(tx);
        let cancel_token = CancellationToken::new();

        let state_stream = stream::iter(vec![
            Ok("active".to_string()),
            Ok("active".to_string()),
            Ok("inactive".to_string()),
        ]);

        let result = run_active_state_monitor(
            state_stream,
            &MockServiceProxy,
            cancel_token,
            converter,
            "test.service",
            UnitActiveState::Inactive,
        )
        .await;
        assert!(result.is_err());

        // Initial state first, then every stream value verbatim (no dedup); the
        // repeated active is later collapsed by transition_to's same-state no-op.
        assert!(matches!(
            rx.recv().await,
            Some(ExecutionStatus::Inactive(_))
        ));
        assert!(matches!(rx.recv().await, Some(ExecutionStatus::Active(_))));
        assert!(matches!(rx.recv().await, Some(ExecutionStatus::Active(_))));
        assert!(matches!(
            rx.recv().await,
            Some(ExecutionStatus::Inactive(_))
        ));
        assert_eq!(rx.try_recv().unwrap_err(), mpsc::error::TryRecvError::Empty);
    }

    #[tokio::test]
    async fn test_run_active_state_monitor_propagates_stream_error() {
        use futures_util::stream;

        let (tx, _rx) = mpsc::channel::<ExecutionStatus>(10);
        let (converter, _handle) = spawn::<ActiveStateConverterActor>(tx);
        let cancel_token = CancellationToken::new();

        let state_stream = stream::iter(vec![
            Ok("active".to_string()),
            Err(anyhow::anyhow!("D-Bus connection lost")),
        ]);

        let result = run_active_state_monitor(
            state_stream,
            &MockServiceProxy,
            cancel_token,
            converter,
            "test.service",
            UnitActiveState::Inactive,
        )
        .await;
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("D-Bus connection lost")
        );
    }

    #[tokio::test]
    async fn test_run_active_state_monitor_respects_cancellation() {
        use std::time::Duration;

        let (tx, _rx) = mpsc::channel::<ExecutionStatus>(10);
        let (converter, _handle) = spawn::<ActiveStateConverterActor>(tx);
        let cancel_token = CancellationToken::new();
        let canceler = cancel_token.clone();

        cancel_token.cancel();

        let result = tokio::time::timeout(
            Duration::from_secs(1),
            run_active_state_monitor(
                futures_util::stream::pending(),
                &MockServiceProxy,
                canceler,
                converter,
                "test.service",
                UnitActiveState::Inactive,
            ),
        )
        .await;

        assert!(result.is_ok(), "Should exit immediately on cancellation");
        assert!(result.unwrap().is_ok());
    }

    #[tokio::test]
    async fn test_run_active_state_monitor_handles_failed_state() {
        use futures_util::stream;

        let (tx, mut rx) = mpsc::channel::<ExecutionStatus>(10);
        let (converter, _handle) = spawn::<ActiveStateConverterActor>(tx);
        let cancel_token = CancellationToken::new();

        let state_stream = stream::iter(vec![Ok("failed".to_string())]);

        let result = run_active_state_monitor(
            state_stream,
            &MockServiceProxy,
            cancel_token,
            converter,
            "test.service",
            UnitActiveState::Inactive,
        )
        .await;
        // Stream exhausted after delivering "failed" → bail on None
        assert!(result.is_err());

        // Initial Inactive emitted first, then the failed state as Active.
        assert!(matches!(
            rx.recv().await,
            Some(ExecutionStatus::Inactive(_))
        ));
        let status = rx.recv().await.unwrap();
        assert!(matches!(status, ExecutionStatus::Active(_)));
    }

    #[tokio::test]
    async fn test_run_active_state_monitor_initial_active_forwards_coalesced_exit() {
        use futures_util::stream;

        let (tx, mut rx) = mpsc::channel::<ExecutionStatus>(10);
        let (converter, _handle) = spawn::<ActiveStateConverterActor>(tx);
        let cancel_token = CancellationToken::new();

        // Adopt path: monitor starts at Active, then the only delivered value is a
        // coalesced "inactive". The initial Active is emitted first, then the exit
        // Inactive is forwarded so the package leaves Running.
        let state_stream = stream::iter(vec![Ok("inactive".to_string())]);

        let result = run_active_state_monitor(
            state_stream,
            &MockServiceProxy,
            cancel_token,
            converter,
            "test.service",
            UnitActiveState::Active,
        )
        .await;
        assert!(result.is_err());

        assert!(matches!(rx.recv().await, Some(ExecutionStatus::Active(_))));
        assert!(matches!(
            rx.recv().await,
            Some(ExecutionStatus::Inactive(_))
        ));
        assert_eq!(rx.try_recv().unwrap_err(), mpsc::error::TryRecvError::Empty);
    }
}
