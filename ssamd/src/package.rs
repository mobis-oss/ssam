// Copyright 2025-2026 Hyundai Mobis Co., Ltd.
// SPDX-License-Identifier: Apache-2.0

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::executor::container::ContainerCommand;
use crate::executor::{self, CommandArguments, ContainerRuntime, ExecutionResult, ExecutionStatus};
use crate::network::NetworkManager;
use crate::package_volume::messages::GetQuotaInfo;
use crate::package_volume::{PackageFsBackend, PackageVolume, PackageVolumeManagerActor};
use crate::utils::{timeline_complete, timeline_start};

use anyhow::Context as _;
use libssam::container::NetworkMode;
use libssam::ssam_package::PackageFile;
use libssam::ssam_package::ssam_pkg_info::{self, BrokenReason};
use libssam::ssam_package::ssam_pkg_metadata::PackageMetadata;
use rsactor::ActorRef;
use strum::Display;
use tokio::sync::mpsc;

mod transition;
use transition::PackageTransitioner;

mod state_machine;
use state_machine::TransitionManager;
#[cfg(test)]
use state_machine::{TransitionManagerActor, transition_messages};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Display)]
pub enum PackageStatus {
    Verified,
    SettingUp,
    Ready,
    StartRequested,
    #[strum(to_string = "Running({0})")]
    Running(String),
    StopRequested,
    CleaningUp,
    Cleaned,
    Upgrading,
    #[strum(to_string = "Error({0})")]
    Error(String),
    #[strum(to_string = "Broken({0})")]
    Broken(BrokenReason),
}

#[derive(Debug, strum::Display)]
pub enum PackagePhase {
    Parse,
    InitExecutor,
    Mount,
    PrepareCommand,
    Start,
    Stop,
    Unmount,
    #[strum(to_string = "{0}")]
    Custom(String),
}

impl From<PackagePhase> for String {
    fn from(phase: PackagePhase) -> Self {
        phase.to_string()
    }
}

#[derive(Debug)]
pub(crate) struct InvalidTransition {
    from: PackageStatus,
    to: PackageStatus,
}

impl std::error::Error for InvalidTransition {}

impl std::fmt::Display for InvalidTransition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Invalid transition from {} to {}", self.from, self.to)
    }
}

#[derive(Debug)]
pub struct PackageContext {
    path: PathBuf,
    package_file: PackageFile,
}

impl PackageContext {
    pub fn new(package_path: impl AsRef<Path>, package_file: PackageFile) -> Self {
        let path = package_path.as_ref().to_path_buf();
        Self { path, package_file }
    }

    #[must_use]
    pub fn package_file(&self) -> &PackageFile {
        &self.package_file
    }

    #[must_use]
    pub fn package_file_path(&self) -> &Path {
        &self.path
    }

    #[must_use]
    pub fn get_name(&self) -> &str {
        self.package_file.metadata().get_package_name()
    }

    #[must_use]
    pub fn is_autostart(&self) -> bool {
        self.package_file
            .metadata()
            .get_package_autostart()
            .copied()
            .unwrap_or(true)
    }
}

#[derive(Debug)]
struct ExecutorInitArgs {
    package_name: String,
    execution_type: executor::ExecutorType,
    state_sender: mpsc::Sender<ExecutionStatus>,
}

#[derive(Debug)]
struct LazyExecutor {
    command: Arc<dyn CommandArguments>,
    init_args: tokio::sync::RwLock<Option<ExecutorInitArgs>>,
    cell: tokio::sync::OnceCell<executor::PackageExecutor>,
}

impl LazyExecutor {
    fn new(command: Arc<dyn CommandArguments>, init_args: ExecutorInitArgs) -> Self {
        Self {
            command,
            init_args: tokio::sync::RwLock::new(Some(init_args)),
            cell: tokio::sync::OnceCell::new(),
        }
    }

    fn command(&self) -> &Arc<dyn CommandArguments> {
        &self.command
    }

    async fn get(&self) -> anyhow::Result<executor::PackageExecutor> {
        let executor = if let Some(executor) = self.cell.get() {
            Ok(executor)
        } else {
            self.cell
                .get_or_try_init(async || {
                    let mut lock = self.init_args.write().await;
                    let init_args = lock.take().expect("Already initialized");

                    let ExecutorInitArgs {
                        package_name,
                        execution_type,
                        state_sender,
                    } = init_args;

                    executor::PackageExecutor::new(
                        package_name,
                        self.command.clone(),
                        execution_type,
                        state_sender,
                    )
                    .await
                })
                .await
        };
        // Clone is safe as the contents of the ExecutorImpl is Arc
        executor.cloned()
    }
}

/// Internal transition orchestrator that manages package state transitions.
///
/// This struct coordinates mount/unmount operations through [`PackageFsBackend`]
/// and synchronizes timeline state across the package lifecycle. It implements
/// the [`PackageTransitioner`] trait to provide standardized access to volume and
/// timeline information.
///
/// # Responsibility
/// - Delegates volume mount/unmount to [`PackageFsBackend`]
/// - Maintains the connection between package name and executor
///
/// # Ownership Model
/// - Owns an `Arc<dyn PackageFsBackend>` for package filesystem operations
/// - Does NOT own the executor (holds `Arc` reference)
#[derive(Debug)]
struct DefaultPackageTransitioner {
    package_name: String,
    executor: Arc<LazyExecutor>,
    pkgfs_handle: Arc<dyn PackageFsBackend>,
    network: Option<NetworkManager>,
    container_interface: String,
}

impl DefaultPackageTransitioner {
    pub(crate) fn new(
        package_name: String,
        executor: Arc<LazyExecutor>,
        pkgfs_handle: Arc<dyn PackageFsBackend>,
        network: Option<NetworkManager>,
        container_interface: String,
    ) -> Self {
        Self {
            package_name,
            executor,
            pkgfs_handle,
            network,
            container_interface,
        }
    }

    fn bridge_network(&self) -> Option<&NetworkManager> {
        self.network.as_ref()
    }
}

#[async_trait::async_trait]
impl PackageTransitioner for DefaultPackageTransitioner {
    async fn setup(&self) -> anyhow::Result<()> {
        log::debug!("Setting up package {}", self.package_name);

        let executor = Arc::clone(&self.executor);

        let package_name_clone = self.package_name.clone();
        // init executor in this point with another task to save time
        let executor_handle = tokio::spawn(async move {
            timeline_start(&package_name_clone, PackagePhase::InitExecutor);
            // force init
            let executor = executor.get().await;
            timeline_complete(&package_name_clone, PackagePhase::InitExecutor);
            executor
        });

        let pkgfs = Arc::clone(&self.pkgfs_handle);
        let pkg_name = self.package_name.clone();
        let mount_handle = tokio::spawn(async move {
            timeline_start(&pkg_name, PackagePhase::Mount);
            let mount_result = match pkgfs.mount().await {
                result @ Ok(()) => result,
                Err(e) => {
                    log::warn!("{pkg_name}: Retrying mount of package fs after failure: {e:#?}");

                    let unmount_result = pkgfs.unmount().with_context(|| {
                        format!("{pkg_name}: Failed to unmount package fs during retry")
                    });

                    if unmount_result.is_err() {
                        unmount_result
                    } else {
                        pkgfs.mount().await
                    }
                }
            }
            .with_context(|| format!("{pkg_name}: Failed to mount package fs"));

            timeline_complete(&pkg_name, PackagePhase::Mount);
            mount_result
        });

        let command = Arc::clone(self.executor.command());
        let prep_name = self.package_name.clone();
        let prepare_handle = tokio::spawn(async move {
            timeline_start(&prep_name, PackagePhase::PrepareCommand);
            let r = command.prepare().await;
            timeline_complete(&prep_name, PackagePhase::PrepareCommand);
            r
        });

        let netns_net = self.bridge_network().cloned();
        let netns_pkg = self.package_name.clone();
        let netns_iface = self.container_interface.clone();
        let netns_handle = tokio::spawn(async move {
            match netns_net {
                Some(net) => {
                    net.create_netns(&netns_pkg).await?;
                    net.attach(&netns_pkg, &netns_iface).await.map(|_| ())
                }
                None => Ok(()),
            }
        });

        let (executor_result, mount_result, prepare_result, netns_result) =
            tokio::try_join!(executor_handle, mount_handle, prepare_handle, netns_handle)
                .with_context(|| format!("{}: Cannot join the task on setup", self.package_name))?;
        let _ = executor_result
            .with_context(|| format!("{}: Error on initialize LazyExecutor", self.package_name))?;
        prepare_result
            .with_context(|| format!("{}: Error on prepare container bundle", self.package_name))?;
        netns_result
            .with_context(|| format!("{}: Error on attach bridge network", self.package_name))?;

        mount_result.with_context(|| format!("{}: Error on mount", self.package_name))
    }

    async fn cleanup(&self) -> anyhow::Result<()> {
        log::debug!("Cleaning up package {}", self.package_name);

        if let Some(net) = self.bridge_network() {
            let _ = net.detach(&self.package_name).await.inspect_err(|e| {
                log::warn!("{}: detach during cleanup failed: {e:#}", self.package_name);
            });
            let _ = net
                .destroy_netns(&self.package_name)
                .await
                .inspect_err(|e| {
                    log::warn!(
                        "{}: destroy_netns during cleanup failed: {e:#}",
                        self.package_name
                    );
                });
        }

        timeline_start(&self.package_name, PackagePhase::Unmount);
        let result = self
            .pkgfs_handle
            .unmount()
            .with_context(|| format!("{}: Failed to unmount package fs", self.package_name));
        timeline_complete(&self.package_name, PackagePhase::Unmount);
        result
    }

    async fn start(&self) -> anyhow::Result<ExecutionResult> {
        let pkg_name = self.package_name.as_str();
        log::debug!("Starting package {pkg_name}");
        timeline_start(&self.package_name, PackagePhase::Start);

        let executor = self.executor.get().await?;
        let result = executor.start().await;

        timeline_complete(&self.package_name, PackagePhase::Start);
        log::debug!("Package {pkg_name} starting job has finished with result {result:?}");
        result
    }

    async fn stop(&self) -> anyhow::Result<ExecutionResult> {
        let pkg_name = self.package_name.as_str();
        log::debug!("Stopping package {pkg_name}");

        timeline_start(&self.package_name, PackagePhase::Stop);
        let executor = self.executor.get().await?;
        let result = executor.stop().await;

        timeline_complete(&self.package_name, PackagePhase::Stop);
        log::debug!("Package {pkg_name} stopping job has finished with result {result:?}");
        result
    }

    async fn teardown(&self) -> anyhow::Result<()> {
        log::debug!("Teardown package {}", self.package_name);

        if let Some(executor) = self.executor.cell.get()
            && let Err(e) = executor.clone().teardown().await
        {
            log::warn!(
                "{}: executor teardown failed (continuing with unmount): {e:#}",
                self.package_name
            );
        }

        // Unconditional: teardown only runs on remove/shutdown, and a leftover
        // netns/veth would collide with a reinstall or fresh daemon start.
        if let Some(net) = self.bridge_network() {
            let _ = net.detach(&self.package_name).await.inspect_err(|e| {
                log::warn!(
                    "{}: detach during teardown failed: {e:#}",
                    self.package_name
                );
            });
            let _ = net
                .destroy_netns(&self.package_name)
                .await
                .inspect_err(|e| {
                    log::warn!(
                        "{}: destroy_netns during teardown failed: {e:#}",
                        self.package_name
                    );
                });
        }

        self.pkgfs_handle
            .unmount()
            .with_context(|| format!("{}: Failed to unmount package fs", self.package_name))
    }

    fn get_name(&self) -> &str {
        &self.package_name
    }
}

/// High-level package lifecycle manager that orchestrates state transitions
/// and container execution.
///
/// `Package` is the primary public interface for managing an installed SSAM package.
/// It coordinates state machine transitions (Installed → Prepared → Running → Stopped),
/// delegates volume operations through internal components, and manages OCI container
/// execution via the executor subsystem.
///
/// # Architecture
/// - Owns [`PackageContext`] (metadata, paths, policies)
/// - Coordinates with [`TransitionManager`] (state machine actor)
/// - Manages async message processing via background task
///
/// # State Machine Integration
/// The transition manager internally uses [`DefaultPackageTransitioner`] which delegates
/// mount/unmount to volume components, ensuring clean separation between
/// package lifecycle logic and volume management mechanics.
///
/// # Thread Safety
/// All state mutations are serialized through the actor model. Public methods
/// send messages to the state machine actor, ensuring safe concurrent access.
#[derive(Debug)]
pub struct Package {
    context: PackageContext,
    transition_mgr: TransitionManager,
    volume_manager_ref: ActorRef<PackageVolumeManagerActor>,
    _receiver_handle: tokio::task::JoinHandle<anyhow::Result<()>>,
}

impl Package {
    /// # Errors
    ///
    /// Returns an error if building the OCI runtime config or systemd service info fails.
    pub fn new(
        context: PackageContext,
        pkg_volume: &PackageVolume,
        volume_manager_ref: ActorRef<PackageVolumeManagerActor>,
        network: Option<NetworkManager>,
    ) -> anyhow::Result<Self> {
        let (sender, mut receiver) = mpsc::channel(8);

        let pkg_name = context.get_name().to_owned();

        let package_file = context.package_file();
        let metadata = package_file.metadata();

        // Authoritative network-mode parse: resolved once here and passed to both
        // the container command and the transitioner so they never diverge.
        let mut network_mode = metadata
            .get_container_network_mode()
            .map(|m| m.parse::<NetworkMode>())
            .transpose()
            .context("Invalid network mode in package config")?
            .unwrap_or(NetworkMode::Host);

        let container_interface = metadata
            .get_container_network_bridge_interface_name()
            .map_or_else(
                || crate::network::DEFAULT_CONTAINER_INTERFACE.to_owned(),
                std::string::ToString::to_string,
            );

        // A package may request bridge mode while the daemon network is disabled.
        // Rather than rejecting it, fall back to host networking so the package
        // still runs, warning that the requested isolation is lost.
        if network_mode == NetworkMode::Bridge && network.is_none() {
            log::warn!(
                "{pkg_name}: bridge network mode requested but the daemon network is disabled; \
                 falling back to host network mode"
            );
            network_mode = NetworkMode::Host;
        }

        // netns is needed only when the (container) command runs in bridge mode.
        // Gate the network manager here at the command fork so the transitioner
        // stays network-mode-agnostic; a future non-container command passes None.
        let effective_network = if network_mode == NetworkMode::Bridge {
            network
        } else {
            None
        };

        let command: Arc<dyn CommandArguments> = Arc::new(ContainerCommand::from_package(
            pkg_name.clone(),
            ContainerRuntime::CRun,
            package_file,
            pkg_volume,
            network_mode,
        ));

        let service_info = executor::systemd::ServiceInfo::new_from_metadata(metadata)?;

        let execution_type = executor::ExecutorType::Systemd(service_info);

        let init_args = ExecutorInitArgs {
            package_name: pkg_name.clone(),
            execution_type,
            state_sender: sender,
        };

        let executor = Arc::new(LazyExecutor::new(command, init_args));

        let pkgfs_handle = pkg_volume.packagefs();
        let ops = DefaultPackageTransitioner::new(
            pkg_name.clone(),
            executor,
            pkgfs_handle,
            effective_network,
            container_interface,
        );
        let transition_mgr = TransitionManager::new(Box::new(ops));
        let transition_mgr_ref = transition_mgr.clone();

        let receiver_handle = tokio::spawn(async move {
            let result: anyhow::Result<()> = loop {
                if let Some(state) = receiver.recv().await {
                    log::debug!("{pkg_name}: Received active state: {state}");
                    let status = match state {
                        ExecutionStatus::Active(state) => PackageStatus::Running(state.to_string()),
                        ExecutionStatus::Inactive(_) => PackageStatus::Ready,
                    };

                    if let Err(e) = transition_mgr_ref.transition_to(status).await {
                        log::error!("{pkg_name}: Failed to transit status: {e:#}");
                        break Err(e);
                    }
                } else {
                    break Err(anyhow::anyhow!("Channel closed"));
                }
            };
            if let Err(err) = &result {
                log::error!("Package {pkg_name} failed to handle the receiver: {err:#}");

                let err_str = format!("{err:#}");

                transition_mgr_ref
                    .transition_to(PackageStatus::Error(err_str))
                    .await
                    .with_context(|| {
                        format!("Failed to set status of package {pkg_name} to error")
                    })?;
            }
            result
        });

        let pkg = Self {
            context,
            transition_mgr,
            volume_manager_ref,
            _receiver_handle: receiver_handle,
        };

        pkg.initialize();

        Ok(pkg)
    }

    fn initialize(&self) {
        let transition_mgr = self.transition_mgr.clone();
        let autostart = self.is_autostart();
        let pkg_name = self.context.get_name().to_owned();
        tokio::spawn(async move {
            if let Err(e) = transition_mgr.request_setup().await {
                log::warn!("Ignore the error on request_setup package {pkg_name}: {e:#}");
            }
            if autostart && let Err(e) = transition_mgr.request_start().await {
                log::warn!("Ignore the error on autostart package {pkg_name}: {e:#}");
            }
        });
    }

    /// # Errors
    ///
    /// Returns an error if the transition to `SettingUp` state fails.
    pub async fn setup(&self) -> anyhow::Result<()> {
        self.transition_mgr.request_setup().await
    }

    /// # Errors
    ///
    /// Returns an error if the transition to `CleaningUp` state fails.
    pub async fn cleanup(&self) -> anyhow::Result<()> {
        self.transition_mgr.request_cleanup().await
    }

    /// # Errors
    ///
    /// Returns an error if the transition to `StartRequested` state fails.
    pub async fn request_start(&self) -> anyhow::Result<()> {
        self.transition_mgr.request_start().await
    }

    /// # Errors
    ///
    /// Returns an error if the transition to `StopRequested` state fails.
    pub async fn request_stop(&self) -> anyhow::Result<()> {
        self.transition_mgr.request_stop().await
    }

    /// # Errors
    ///
    /// Returns an error if stopping the container or unmounting the package
    /// filesystem fails during the upgrade transition.
    pub async fn prepare_upgrade(&self) -> anyhow::Result<()> {
        self.transition_mgr.prepare_upgrade().await
    }

    /// # Errors
    ///
    /// Returns an error if stopping the container or unmounting the package
    /// filesystem fails.
    pub async fn teardown(&self) -> anyhow::Result<()> {
        let status = self.get_status().await?;
        // Nothing to tear down — these states have no mounted filesystem.
        if matches!(
            status,
            PackageStatus::Verified | PackageStatus::Cleaned | PackageStatus::Upgrading
        ) {
            return Ok(());
        }
        self.transition_mgr.teardown().await
    }

    #[must_use]
    pub fn get_package_file_path(&self) -> &Path {
        self.context.package_file_path()
    }

    #[must_use]
    pub fn get_name(&self) -> &str {
        self.context.get_name()
    }

    #[must_use]
    pub fn is_autostart(&self) -> bool {
        self.context.is_autostart()
    }

    #[must_use]
    pub fn get_version(&self) -> semver::Version {
        let metadata: &PackageMetadata = self.context.package_file.metadata();
        metadata.version()
    }

    #[must_use]
    pub fn get_metadata(&self) -> &PackageMetadata {
        self.context.package_file.metadata()
    }

    /// # Errors
    ///
    /// Returns an error if the transition manager actor is unavailable.
    pub async fn get_status(&self) -> anyhow::Result<PackageStatus> {
        self.transition_mgr.get_status().await
    }

    /// # Errors
    ///
    /// Returns an error if the status or quota info cannot be retrieved.
    pub async fn get_package_info(&self) -> anyhow::Result<ssam_pkg_info::PackageInfo> {
        let package_metadata_ref: &PackageMetadata = self.context.package_file.metadata();

        let package_status = self
            .get_status()
            .await
            .with_context(|| {
                format!(
                    "Failed to get status of package {}",
                    self.context.get_name()
                )
            })?
            .to_string();
        let package_name = self.get_name().to_owned();

        let quota_info = if let Some(limit) = self
            .volume_manager_ref
            .ask(GetQuotaInfo {
                name: package_name.clone(),
            })
            .await
            .context("Failed to communicate with PackageVolumeManager Actor")?
            .context("Failed to get quota info")?
        {
            ssam_pkg_info::QuotaInformation {
                enabled: true,
                limit,
            }
        } else {
            log::debug!("No quota info for package {package_name}");
            ssam_pkg_info::QuotaInformation {
                enabled: false,
                limit: 0,
            }
        };

        Ok(ssam_pkg_info::PackageInfo {
            package_metadata: package_metadata_ref.clone(),
            package_status,
            package_name,
            package_path: self.context.package_file_path().display().to_string(),
            quota_info,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // Mock implementation for testing
    #[derive(Debug)]
    pub(crate) struct MockExecutionState {
        state: String,
    }

    impl MockExecutionState {
        pub(crate) fn new(state: &str) -> Arc<Self> {
            Arc::new(Self {
                state: state.to_string(),
            })
        }
    }

    impl executor::ExecutionState for MockExecutionState {}

    impl std::fmt::Display for MockExecutionState {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "{}", self.state)
        }
    }

    // MockPackageTransitioner uses 4 bool fields to control test scenario outcomes; struct_excessive_bools is intentional.
    #[allow(clippy::struct_excessive_bools)]
    #[derive(Debug)]
    pub(crate) struct MockPackageTransitioner {
        pkg_name: String,
        setup_should_fail: bool,
        start_should_fail: bool,
        stop_should_fail: bool,
        cleanup_should_fail: bool,
        start_result_state: String,
        stop_result_state: String,
    }

    impl MockPackageTransitioner {
        pub(crate) fn new(pkg_name: String) -> Self {
            Self {
                pkg_name,
                setup_should_fail: false,
                start_should_fail: false,
                stop_should_fail: false,
                cleanup_should_fail: false,
                start_result_state: "running".to_string(),
                stop_result_state: "stopped".to_string(),
            }
        }

        pub(crate) fn with_setup_error(mut self, _error: &str) -> Self {
            self.setup_should_fail = true;
            self
        }

        pub(crate) fn with_start_error(mut self) -> Self {
            self.start_should_fail = true;
            self
        }

        pub(crate) fn with_stop_error(mut self) -> Self {
            self.stop_should_fail = true;
            self
        }

        pub(crate) fn with_cleanup_error(mut self) -> Self {
            self.cleanup_should_fail = true;
            self
        }
    }

    #[async_trait::async_trait]
    impl PackageTransitioner for MockPackageTransitioner {
        async fn setup(&self) -> anyhow::Result<()> {
            if self.setup_should_fail {
                Err(anyhow::anyhow!("Setup failed"))
            } else {
                Ok(())
            }
        }

        async fn cleanup(&self) -> anyhow::Result<()> {
            if self.cleanup_should_fail {
                Err(anyhow::anyhow!("Cleanup failed"))
            } else {
                Ok(())
            }
        }

        async fn start(&self) -> anyhow::Result<ExecutionResult> {
            if self.start_should_fail {
                Err(anyhow::anyhow!("Start failed"))
            } else {
                Ok(ExecutionResult::Success(executor::ExecutionStatus::Active(
                    MockExecutionState::new(&self.start_result_state),
                )))
            }
        }

        async fn stop(&self) -> anyhow::Result<ExecutionResult> {
            if self.stop_should_fail {
                Err(anyhow::anyhow!("Stop failed"))
            } else {
                Ok(ExecutionResult::Success(
                    executor::ExecutionStatus::Inactive(MockExecutionState::new(
                        &self.stop_result_state,
                    )),
                ))
            }
        }

        async fn teardown(&self) -> anyhow::Result<()> {
            Ok(())
        }

        fn get_name(&self) -> &str {
            &self.pkg_name
        }
    }

    #[tokio::test]
    async fn test_transition_manager_with_mock_ops() {
        // Test successful setup transition
        let mock_ops = Box::new(MockPackageTransitioner::new("test-package".to_string()));
        let mut actor = TransitionManagerActor::new(mock_ops);

        // Initial state should be Verified
        assert_eq!(actor.status, PackageStatus::Verified);

        // Test transition to SettingUp (which should complete to Ready)
        let result = actor.transition_to(PackageStatus::SettingUp).await;
        assert!(result.is_ok());
        assert_eq!(actor.status, PackageStatus::Ready);
    }

    #[tokio::test]
    async fn test_transition_manager_with_setup_failure() {
        // Test setup failure
        let mock_ops = Box::new(
            MockPackageTransitioner::new("test-package".to_string())
                .with_setup_error("Setup failed"),
        );
        let mut actor = TransitionManagerActor::new(mock_ops);

        // Test transition to SettingUp (which should fail and go to Error)
        let result = actor.transition_to(PackageStatus::SettingUp).await;
        assert!(result.is_err());
        assert!(matches!(actor.status, PackageStatus::Error(_)));
    }

    #[tokio::test]
    async fn test_no_transition_needed() {
        let mock_ops = Box::new(MockPackageTransitioner::new("test-package".to_string()));
        let mut actor = TransitionManagerActor::new(mock_ops);

        // Transition to same state should succeed without doing anything
        let result = actor.transition_to(PackageStatus::Verified).await;
        assert!(result.is_ok());
        assert_eq!(actor.status, PackageStatus::Verified);
    }

    #[tokio::test]
    async fn test_transition_manager_actor_message_handlers() {
        let mock_ops = Box::new(MockPackageTransitioner::new("test-package".to_string()));
        let (actor_ref, _) =
            rsactor::spawn::<TransitionManagerActor>(TransitionManagerActor::new(mock_ops));

        // Test GetStatus message
        let status = actor_ref
            .ask(transition_messages::GetStatus)
            .await
            .expect("Should get status");
        assert_eq!(status, PackageStatus::Verified);

        // Test RequestTransition message
        let result = actor_ref
            .ask(transition_messages::RequestTransition {
                target: PackageStatus::SettingUp,
            })
            .await
            .expect("Should handle transition request");
        assert!(result.is_ok());

        // Verify status changed to Ready after setup transition
        let status = actor_ref
            .ask(transition_messages::GetStatus)
            .await
            .expect("Should get status");
        assert_eq!(status, PackageStatus::Ready);

        // Test Teardown message
        let result = actor_ref
            .ask(transition_messages::Teardown)
            .await
            .expect("Should handle teardown");
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_transition_manager_actor_invalid_transition() {
        let mock_ops = Box::new(MockPackageTransitioner::new("test-package".to_string()));
        let mut actor = TransitionManagerActor::new(mock_ops);

        // Try invalid transition from Verified to Running
        let result = actor
            .transition_to(PackageStatus::Running("active".to_string()))
            .await;
        assert!(result.is_err());

        // Status should remain unchanged
        assert_eq!(actor.status, PackageStatus::Verified);
    }

    #[tokio::test]
    async fn test_transition_manager_actor_start_and_stop_flow() {
        let mock_ops = Box::new(MockPackageTransitioner::new("test-package".to_string()));
        let mut actor = TransitionManagerActor::new(mock_ops);

        // Setup -> Ready
        let result = actor.transition_to(PackageStatus::SettingUp).await;
        assert!(result.is_ok());
        assert_eq!(actor.status, PackageStatus::Ready);

        // Start -> Running
        let result = actor.transition_to(PackageStatus::StartRequested).await;
        assert!(result.is_ok());
        assert!(matches!(actor.status, PackageStatus::Running(_)));

        // Stop -> Ready
        let result = actor.transition_to(PackageStatus::StopRequested).await;
        assert!(result.is_ok());
        assert_eq!(actor.status, PackageStatus::Ready);
    }

    #[tokio::test]
    async fn test_transition_manager_actor_cleanup_flow() {
        let mock_ops = Box::new(MockPackageTransitioner::new("test-package".to_string()));
        let mut actor = TransitionManagerActor::new(mock_ops);

        // Setup first
        let result = actor.transition_to(PackageStatus::SettingUp).await;
        assert!(result.is_ok());
        assert_eq!(actor.status, PackageStatus::Ready);

        // Cleanup -> Cleaned
        let result = actor.transition_to(PackageStatus::CleaningUp).await;
        assert!(result.is_ok());
        assert_eq!(actor.status, PackageStatus::Cleaned);
    }

    mod transition_manager_handler_tests {

        use super::*;

        #[tokio::test]
        async fn test_transition_manager_handler_creation() {
            let mock_ops = Box::new(MockPackageTransitioner::new("test-package".to_string()));
            let _handler = TransitionManager::new(mock_ops);
        }

        #[tokio::test]
        async fn test_transition_manager_handler_setup() {
            let mock_ops = Box::new(MockPackageTransitioner::new("test-package".to_string()));
            let handler = TransitionManager::new(mock_ops);

            let result = handler.request_setup().await;
            assert!(result.is_ok());

            // Verify status changed to Ready
            let status = handler.get_status().await.expect("Should get status");
            assert_eq!(status, PackageStatus::Ready);
        }

        #[tokio::test]
        async fn test_transition_manager_handler_setup_failure() {
            let mock_ops = Box::new(
                MockPackageTransitioner::new("test-package".to_string())
                    .with_setup_error("Setup failed"),
            );
            let handler = TransitionManager::new(mock_ops);

            let result = handler.request_setup().await;
            assert!(result.is_err());

            // Verify status changed to Error
            let status = handler.get_status().await.expect("Should get status");
            assert!(matches!(status, PackageStatus::Error(_)));
        }

        #[tokio::test]
        async fn test_transition_manager_handler_start() {
            let mock_ops = Box::new(MockPackageTransitioner::new("test-package".to_string()));
            let handler = TransitionManager::new(mock_ops);

            // Setup first
            handler.request_setup().await.expect("Setup should succeed");

            // Then start
            let result = handler.request_start().await;
            assert!(result.is_ok());

            // Verify status changed to Running
            let status = handler.get_status().await.expect("Should get status");
            assert!(matches!(status, PackageStatus::Running(_)));
        }

        #[tokio::test]
        async fn test_transition_manager_handler_start_failure() {
            let mock_ops = Box::new(
                MockPackageTransitioner::new("test-package".to_string()).with_start_error(),
            );
            let handler = TransitionManager::new(mock_ops);

            // Setup first
            handler.request_setup().await.expect("Setup should succeed");

            // Then try to start (should fail)
            let result = handler.request_start().await;
            assert!(result.is_err());

            // Verify status changed to Error
            let status = handler.get_status().await.expect("Should get status");
            assert!(matches!(status, PackageStatus::Error(_)));
        }

        #[tokio::test]
        async fn test_transition_manager_handler_stop() {
            let mock_ops = Box::new(MockPackageTransitioner::new("test-package".to_string()));
            let handler = TransitionManager::new(mock_ops);

            // Setup and start first
            handler.request_setup().await.expect("Setup should succeed");
            handler.request_start().await.expect("Start should succeed");

            // Then stop
            let result = handler.request_stop().await;
            assert!(result.is_ok());

            // Verify status changed to Ready
            let status = handler.get_status().await.expect("Should get status");
            assert_eq!(status, PackageStatus::Ready);
        }

        #[tokio::test]
        async fn test_transition_manager_handler_stop_failure() {
            let mock_ops = Box::new(
                MockPackageTransitioner::new("test-package".to_string()).with_stop_error(),
            );
            let handler = TransitionManager::new(mock_ops);

            // Setup and start first
            handler.request_setup().await.expect("Setup should succeed");
            handler.request_start().await.expect("Start should succeed");

            // Then try to stop (should fail)
            let result = handler.request_stop().await;
            assert!(result.is_err());

            // Verify status changed to Error
            let status = handler.get_status().await.expect("Should get status");
            assert!(matches!(status, PackageStatus::Error(_)));
        }

        #[tokio::test]
        async fn test_transition_manager_handler_cleanup() {
            let mock_ops = Box::new(MockPackageTransitioner::new("test-package".to_string()));
            let handler = TransitionManager::new(mock_ops);

            // Setup first
            handler.request_setup().await.expect("Setup should succeed");

            // Then cleanup
            let result = handler.request_cleanup().await;
            assert!(result.is_ok());

            // Verify status changed to Cleaned
            let status = handler.get_status().await.expect("Should get status");
            assert_eq!(status, PackageStatus::Cleaned);
        }

        #[tokio::test]
        async fn test_transition_manager_handler_cleanup_failure() {
            let mock_ops = Box::new(
                MockPackageTransitioner::new("test-package".to_string()).with_cleanup_error(),
            );
            let handler = TransitionManager::new(mock_ops);

            // Setup first
            handler.request_setup().await.expect("Setup should succeed");

            // Then try to cleanup (should fail)
            let result = handler.request_cleanup().await;
            assert!(result.is_err());

            // Verify status changed to Error
            let status = handler.get_status().await.expect("Should get status");
            assert!(matches!(status, PackageStatus::Error(_)));
        }

        #[tokio::test]
        async fn test_transition_manager_handler_teardown() {
            let mock_ops = Box::new(MockPackageTransitioner::new("test-package".to_string()));
            let handler = TransitionManager::new(mock_ops);

            let result = handler.teardown().await;
            assert!(result.is_ok());
        }

        #[tokio::test]
        async fn test_transition_manager_handler_complex_workflow() {
            let mock_ops = Box::new(MockPackageTransitioner::new("test-package".to_string()));
            let handler = TransitionManager::new(mock_ops);

            // Initial status should be Verified
            let status = handler.get_status().await.expect("Should get status");
            assert_eq!(status, PackageStatus::Verified);

            // Setup
            handler.request_setup().await.expect("Setup should succeed");
            let status = handler.get_status().await.expect("Should get status");
            assert_eq!(status, PackageStatus::Ready);

            // Start
            handler.request_start().await.expect("Start should succeed");
            let status = handler.get_status().await.expect("Should get status");
            assert!(matches!(status, PackageStatus::Running(_)));

            // Stop
            handler.request_stop().await.expect("Stop should succeed");
            let status = handler.get_status().await.expect("Should get status");
            assert_eq!(status, PackageStatus::Ready);

            // Cleanup
            handler
                .request_cleanup()
                .await
                .expect("Cleanup should succeed");
            let status = handler.get_status().await.expect("Should get status");
            assert_eq!(status, PackageStatus::Cleaned);

            // Teardown
            handler.teardown().await.expect("Teardown should succeed");
        }

        #[tokio::test]
        async fn test_transition_manager_handler_ignore_transitions() {
            let mock_ops = Box::new(MockPackageTransitioner::new("test-package".to_string()));
            let handler = TransitionManager::new(mock_ops);

            // Setup to get to Ready state
            handler.request_setup().await.expect("Setup should succeed");
            let status = handler.get_status().await.expect("Should get status");
            assert_eq!(status, PackageStatus::Ready);

            // Request stop while already in Ready state (should be ignored)
            let result = handler.request_stop().await;
            assert!(result.is_ok()); // Should succeed but do nothing

            // Status should remain Ready
            let status = handler.get_status().await.expect("Should get status");
            assert_eq!(status, PackageStatus::Ready);
        }

        #[tokio::test]
        async fn test_prepare_upgrade_from_verified() {
            let mock_ops = Box::new(MockPackageTransitioner::new("test-package".to_string()));
            let handler = TransitionManager::new(mock_ops);

            let status = handler.get_status().await.expect("Should get status");
            assert_eq!(status, PackageStatus::Verified);

            handler
                .prepare_upgrade()
                .await
                .expect("Upgrade from Verified should succeed");

            let status = handler.get_status().await.expect("Should get status");
            assert_eq!(status, PackageStatus::Upgrading);
        }

        #[tokio::test]
        async fn test_prepare_upgrade_from_ready() {
            let mock_ops = Box::new(MockPackageTransitioner::new("test-package".to_string()));
            let handler = TransitionManager::new(mock_ops);

            handler.request_setup().await.expect("Setup should succeed");
            let status = handler.get_status().await.expect("Should get status");
            assert_eq!(status, PackageStatus::Ready);

            handler
                .prepare_upgrade()
                .await
                .expect("Upgrade from Ready should succeed");

            let status = handler.get_status().await.expect("Should get status");
            assert_eq!(status, PackageStatus::Upgrading);
        }

        #[tokio::test]
        async fn test_prepare_upgrade_from_cleaned_fails() {
            let mock_ops = Box::new(MockPackageTransitioner::new("test-package".to_string()));
            let handler = TransitionManager::new(mock_ops);

            handler.request_setup().await.expect("Setup should succeed");
            handler
                .request_cleanup()
                .await
                .expect("Cleanup should succeed");
            let status = handler.get_status().await.expect("Should get status");
            assert_eq!(status, PackageStatus::Cleaned);

            let result = handler.prepare_upgrade().await;
            assert!(result.is_err());
        }

        #[tokio::test]
        async fn test_raw_teardown_succeeds_from_any_state() {
            let mock_ops = Box::new(MockPackageTransitioner::new("test-package".to_string()));
            let handler = TransitionManager::new(mock_ops);

            let status = handler.get_status().await.expect("Should get status");
            assert_eq!(status, PackageStatus::Verified);

            handler.teardown().await.expect("Teardown should succeed");
        }
    }
}
