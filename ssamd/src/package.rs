// Copyright 2025-2026 Hyundai Mobis Co., Ltd.
// SPDX-License-Identifier: Apache-2.0

use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::configuration::Configuration;
use crate::executor::container::ContainerCommand;
use crate::executor::oci_bundle_dir::OciBundleDir;
use crate::executor::{self, ExecuteCommand, ExecutionResult, ExecutionStatus};
use crate::network::NetworkManager;
use crate::package_volume::messages::{AcquireVolume, GetQuotaInfo, WipeVolumeData};
use crate::package_volume::{
    PackageFsBackend, PackageVolume, PackageVolumeManagerActor, PackageVolumeMetadata,
};
use crate::utils::{timeline_complete, timeline_start};

use anyhow::Context as _;
use async_trait::async_trait;
use libssam::PackageLocation;
use libssam::container::NetworkMode;
use libssam::ssam_package::PackageFile;
use libssam::ssam_package::ssam_pkg_info::{self, BrokenReason};
use libssam::ssam_package::ssam_pkg_metadata::PackageMetadata;
use rsactor::ActorRef;
use strum::Display;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

mod state_machine;
use state_machine::{LifecycleBackend, TransitionManager};

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

impl From<ExecutionResult> for PackageStatus {
    fn from(state: ExecutionResult) -> Self {
        match state {
            ExecutionResult::Success(state) | ExecutionResult::Canceled(state) => match state {
                ExecutionStatus::Active(state) => PackageStatus::Running(state.to_string()),
                ExecutionStatus::Inactive(_) => PackageStatus::Ready,
            },
            ExecutionResult::Failure(reason) => PackageStatus::Error(reason),
            ExecutionResult::Timeout => PackageStatus::Error("timeout".to_string()),
        }
    }
}

#[derive(Debug, strum::Display)]
pub enum PackagePhase {
    Parse,
    InitExecutor,
    Mount,
    PrepareCommand,
    AttachNetwork,
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
    package_path: PathBuf,
    package_file: PackageFile,
    network: PackageNetwork,
    config: Arc<Configuration>,
    service_info: executor::systemd::ServiceInfo,
    state_sender: mpsc::Sender<ExecutionStatus>,
    oci_bundle_dir: Arc<OciBundleDir>,
}

impl PackageContext {
    pub(crate) fn new(
        package_path: impl AsRef<Path>,
        package_file: PackageFile,
        network: Option<NetworkManager>,
        config: Arc<Configuration>,
        state_sender: mpsc::Sender<ExecutionStatus>,
    ) -> anyhow::Result<Self> {
        let package_path = package_path.as_ref().to_path_buf();
        // The volume is acquired at setup; validate its metadata now to fail install.
        PackageVolumeMetadata::new(&package_path, &package_file, &config)
            .context("Invalid package volume metadata")?;
        let network = PackageNetwork::new(package_file.metadata(), network)?;
        let service_info =
            executor::systemd::ServiceInfo::new_from_metadata(package_file.metadata())?;
        let metadata = package_file.metadata();
        let oci_bundle_dir = Arc::new(
            OciBundleDir::new(
                Path::new(config.oci_bundle_root()),
                metadata.get_package_name(),
                &metadata.version().to_string(),
            )
            .context("Failed to create OCI bundle directory")?,
        );

        Ok(Self {
            package_path,
            package_file,
            network,
            config,
            service_info,
            state_sender,
            oci_bundle_dir,
        })
    }

    #[must_use]
    pub fn package_file(&self) -> &PackageFile {
        &self.package_file
    }

    #[must_use]
    pub fn package_file_path(&self) -> &Path {
        &self.package_path
    }

    #[must_use]
    pub fn get_name(&self) -> &str {
        self.package_file().metadata().get_package_name()
    }

    #[must_use]
    pub fn is_autostart(&self) -> bool {
        self.package_file()
            .metadata()
            .get_package_autostart()
            .copied()
            .unwrap_or(true)
    }

    #[must_use]
    fn packages_cgroup(&self) -> &str {
        self.config.packages_cgroup()
    }

    #[must_use]
    fn config(&self) -> &Configuration {
        self.config.as_ref()
    }

    #[must_use]
    fn network(&self) -> Option<&NetworkManager> {
        self.network.manager.as_ref()
    }

    #[must_use]
    fn network_mode(&self) -> &NetworkMode {
        &self.network.mode
    }

    #[must_use]
    fn bridge_id(&self) -> &str {
        &self.network.bridge_id
    }
}

#[derive(Debug, Clone)]
struct PackageNetwork {
    manager: Option<NetworkManager>,
    mode: NetworkMode,
    container_interface: String,
    bridge_id: String,
}

impl PackageNetwork {
    fn new(metadata: &PackageMetadata, manager: Option<NetworkManager>) -> anyhow::Result<Self> {
        let package_name = metadata.get_package_name();
        // Authoritative network-mode parse: resolved once here and passed to both
        // the container command and the backend so they never diverge.
        let mode = metadata
            .get_container_network_mode()
            .map(|mode| NetworkMode::parse_mode(mode))
            .transpose()
            .context("Invalid network mode in package config")?
            .unwrap_or(NetworkMode::Host);

        // Reject bridge mode when the daemon network is disabled rather than
        // silently downgrading to host networking, which would misrepresent the
        // isolation the package declared.
        if mode == NetworkMode::Bridge && manager.is_none() {
            anyhow::bail!(
                "{package_name}: bridge network mode requested but the daemon network is disabled"
            );
        }

        let container_interface = metadata
            .get_container_network_bridge_interface_name()
            .map_or_else(
                || crate::network::DEFAULT_CONTAINER_INTERFACE.to_owned(),
                std::string::ToString::to_string,
            );
        // netns is needed only when the (container) command runs in bridge mode;
        // gate the network manager here so the backend stays mode-agnostic.
        let manager = (mode == NetworkMode::Bridge).then_some(manager).flatten();
        // bridge_id groups packages onto a shared bridge: an explicit
        // network_name shares one, its absence gives a private bridge per package.
        let bridge_id = if mode == NetworkMode::Bridge {
            metadata
                .get_container_network_bridge_network_name()
                .cloned()
                .unwrap_or_else(|| package_name.to_owned())
        } else {
            package_name.to_owned()
        };

        Ok(Self {
            manager,
            mode,
            container_interface,
            bridge_id,
        })
    }

    async fn attach(self, package_name: String) -> anyhow::Result<()> {
        let Some(manager) = self.manager else {
            return Ok(());
        };
        manager.create_netns(&package_name).await?;
        manager
            .attach(&package_name, &self.bridge_id, &self.container_interface)
            .await
    }
}

#[derive(Debug)]
struct PackageRuntime {
    executor: executor::PackageExecutor,
    pkgfs_handle: Arc<dyn PackageFsBackend>,
    _command: Arc<dyn ExecuteCommand>,
    _volume: Arc<PackageVolume>,
}

#[derive(Debug)]
struct PackageLifecycle {
    context: Arc<PackageContext>,
    volume_manager_ref: ActorRef<PackageVolumeManagerActor>,
    runtime: Option<PackageRuntime>,
}

impl PackageLifecycle {
    fn spawn_phase<F, T>(name: &str, phase: impl Into<String>, future: F) -> JoinHandle<T>
    where
        F: Future<Output = T> + Send + 'static,
        T: Send + 'static,
    {
        let name = name.to_owned();
        let phase = phase.into();
        tokio::spawn(async move {
            timeline_start(&name, phase.clone());
            let result = future.await;
            timeline_complete(&name, phase);
            result
        })
    }

    async fn acquire_volume(&self) -> anyhow::Result<PackageVolume> {
        let context = self.context.as_ref();
        let volume_meta = PackageVolumeMetadata::new(
            context.package_file_path(),
            context.package_file(),
            context.config(),
        )
        .context("Failed to create package volume metadata")?;
        self.volume_manager_ref
            .ask(AcquireVolume { volume_meta })
            .await
            .context("Failed to communicate with PackageVolumeManager Actor")?
    }

    async fn setup_bundle(
        &self,
        pkgfs_handle: &Arc<dyn PackageFsBackend>,
        command: &Arc<dyn ExecuteCommand>,
    ) -> anyhow::Result<()> {
        let context = self.context.as_ref();
        let package_name = context.get_name();
        let oci_bundle_dir = &context.oci_bundle_dir;
        oci_bundle_dir
            .ensure()
            .with_context(|| format!("{package_name}: Failed to prepare runtime directory"))?;

        let mount_handle = Self::spawn_phase(
            package_name,
            PackagePhase::Mount,
            mount_with_retry(Arc::clone(pkgfs_handle), oci_bundle_dir.pkgfs_path()),
        );
        let prepare_command = Arc::clone(command);
        let prepare_handle =
            Self::spawn_phase(package_name, PackagePhase::PrepareCommand, async move {
                prepare_command.prepare().await
            });
        let network = context.network.clone();
        let netns_handle = Self::spawn_phase(
            package_name,
            PackagePhase::AttachNetwork,
            network.attach(package_name.to_owned()),
        );

        // join, not try_join: a panicked phase must not let cleanup race the others.
        let (mount, prepare, netns) = tokio::join!(mount_handle, prepare_handle, netns_handle);
        [mount, prepare, netns]
            .into_iter()
            .try_for_each(|result| result.context("A bundle setup task panicked")?)
    }

    async fn teardown_bundle(
        &self,
        pkgfs_handle: &Arc<dyn PackageFsBackend>,
    ) -> anyhow::Result<()> {
        let context = self.context.as_ref();
        let oci_bundle_dir = &context.oci_bundle_dir;
        let name = context.get_name();

        // Network release is best-effort; only a failed unmount must block removal.
        if let Some(net) = context.network() {
            if let Err(error) = net.detach(name, context.bridge_id()).await {
                log::warn!("{name}: detach during teardown failed: {error:#}");
            }
            if let Err(error) = net.destroy_netns(name).await {
                log::warn!("{name}: destroy_netns during teardown failed: {error:#}");
            }
        }

        timeline_start(name, PackagePhase::Unmount);
        let unmount = pkgfs_handle
            .unmount(&oci_bundle_dir.pkgfs_path())
            .with_context(|| format!("{name}: Failed to unmount package fs"));
        timeline_complete(name, PackagePhase::Unmount);
        unmount?;

        oci_bundle_dir
            .cleanup()
            .with_context(|| format!("{name}: Failed to cleanup OCI bundle directory"))
    }

    fn runtime(&self) -> anyhow::Result<&PackageRuntime> {
        self.runtime
            .as_ref()
            .with_context(|| format!("{}: package is not provisioned", self.context.get_name()))
    }
}

#[async_trait]
impl LifecycleBackend for PackageLifecycle {
    fn name(&self) -> &str {
        self.context.get_name()
    }

    async fn provision(&mut self) -> anyhow::Result<()> {
        let context = self.context.as_ref();
        let package_name = context.get_name();
        let volume = self.acquire_volume().await?;
        let pkgfs_handle = volume.packagefs();
        let command = ContainerCommand::from_package(
            package_name,
            context.package_file(),
            &volume,
            context.network_mode().clone(),
            context.packages_cgroup(),
            Arc::clone(&context.oci_bundle_dir),
        );

        let executor_handle = Self::spawn_phase(
            package_name,
            PackagePhase::InitExecutor,
            executor::PackageExecutor::new(
                package_name.to_owned(),
                Arc::clone(&command),
                executor::ExecutorType::Systemd(context.service_info.clone()),
                context.state_sender.clone(),
                context.packages_cgroup().to_owned(),
            ),
        );
        let (executor, bundle) =
            tokio::join!(executor_handle, self.setup_bundle(&pkgfs_handle, &command));
        let result = executor
            .context("The executor init task panicked")
            .and_then(|result| result) // Replace with `Result::flatten` once the MSRV reaches 1.89.
            .and_then(|executor| {
                // Kept even if the bundle failed, so removal can retry the teardown.
                self.runtime = Some(PackageRuntime {
                    executor,
                    pkgfs_handle: Arc::clone(&pkgfs_handle),
                    _command: command,
                    _volume: Arc::new(volume),
                });
                bundle
            });
        if let Err(error) = result {
            if let Err(cleanup_error) = self.teardown_bundle(&pkgfs_handle).await {
                log::warn!("{package_name}: Provisioning cleanup failed: {cleanup_error:#}");
            }
            Err(error)
        } else {
            Ok(())
        }
    }

    async fn start(&self) -> PackageStatus {
        let name = self.context.get_name();
        log::debug!("Starting package {name}");
        timeline_start(name, PackagePhase::Start);
        let result = match self.runtime() {
            Ok(runtime) => runtime.executor.start().await,
            Err(error) => Err(error),
        };
        timeline_complete(name, PackagePhase::Start);
        result.map_or_else(
            |error| PackageStatus::Error(format!("{error:#}")),
            PackageStatus::from,
        )
    }

    async fn stop(&self) -> PackageStatus {
        let name = self.context.get_name();
        log::debug!("Stopping package {name}");
        timeline_start(name, PackagePhase::Stop);
        let result = match self.runtime() {
            Ok(runtime) => runtime.executor.stop().await,
            Err(error) => Err(error),
        };
        timeline_complete(name, PackagePhase::Stop);
        result.map_or_else(
            |error| PackageStatus::Error(format!("{error:#}")),
            PackageStatus::from,
        )
    }

    async fn teardown(&mut self) -> anyhow::Result<()> {
        let Some(runtime) = &self.runtime else {
            return Ok(());
        };
        if let Err(error) = runtime.executor.teardown().await {
            let name = self.context.get_name();
            log::warn!("{name}: executor teardown failed (continuing with unmount): {error:#}");
        }
        self.teardown_bundle(&runtime.pkgfs_handle).await?;
        self.runtime = None;
        Ok(())
    }

    async fn wipe_data(&self) -> anyhow::Result<()> {
        self.volume_manager_ref
            .ask(WipeVolumeData {
                name: self.context.get_name().to_owned(),
            })
            .await
            .context("Failed to communicate with PackageVolumeManager Actor")?
    }
}

async fn mount_with_retry(
    pkgfs: Arc<dyn PackageFsBackend>,
    mount_point: PathBuf,
) -> anyhow::Result<()> {
    let name = pkgfs.package_name().to_owned();
    match pkgfs.mount(&mount_point).await {
        result @ Ok(()) => result,
        Err(error) => {
            log::warn!("{name}: retrying package fs mount after failure: {error:#?}");
            pkgfs
                .unmount(&mount_point)
                .with_context(|| format!("{name}: failed to unmount before mount retry"))?;
            pkgfs.mount(&mount_point).await
        }
    }
    .with_context(|| format!("{name}: failed to mount package fs"))
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
/// The transition manager owns lifecycle state and drives resource operations
/// through the [`LifecycleBackend`] implemented by `PackageLifecycle`.
///
/// # Thread Safety
/// All state mutations are serialized through the actor model. Public methods
/// send messages to the state machine actor, ensuring safe concurrent access.
#[derive(Debug)]
pub struct Package {
    context: Arc<PackageContext>,
    transition_mgr: TransitionManager,
    volume_manager_ref: ActorRef<PackageVolumeManagerActor>,
    receiver_handle: tokio::task::JoinHandle<anyhow::Result<()>>,
}

impl Package {
    /// # Errors
    ///
    /// Returns an error if the package network, systemd service or volume
    /// metadata configuration is invalid.
    pub fn new(
        package_path: impl AsRef<Path>,
        package_file: PackageFile,
        volume_manager_ref: ActorRef<PackageVolumeManagerActor>,
        network: Option<NetworkManager>,
        config: Arc<Configuration>,
    ) -> anyhow::Result<Self> {
        let (sender, receiver) = mpsc::channel(8);
        let context = Arc::new(PackageContext::new(
            package_path,
            package_file,
            network,
            config,
            sender,
        )?);
        let pkg_name = context.get_name().to_owned();
        let backend = PackageLifecycle {
            context: Arc::clone(&context),
            volume_manager_ref: volume_manager_ref.clone(),
            runtime: None,
        };
        let transition_mgr = TransitionManager::new(backend);
        let transition_mgr_ref = transition_mgr.clone();

        let receiver_handle = tokio::spawn(Self::run_status_receiver(
            pkg_name,
            receiver,
            transition_mgr_ref,
        ));

        Ok(Self {
            context,
            transition_mgr,
            volume_manager_ref,
            receiver_handle,
        })
    }

    /// Drives a package's status from `ActiveState` monitor events until the
    /// channel closes or a transition failure forces `Error`.
    /// Extracted from `new()` so the constructor stays plain wiring.
    async fn run_status_receiver(
        pkg_name: String,
        mut receiver: mpsc::Receiver<ExecutionStatus>,
        transition_mgr: TransitionManager,
    ) -> anyhow::Result<()> {
        let result: anyhow::Result<()> = loop {
            if let Some(state) = receiver.recv().await {
                log::debug!("{pkg_name}: Received active state: {state}");
                let status = match state {
                    ExecutionStatus::Active(state) => PackageStatus::Running(state.to_string()),
                    ExecutionStatus::Inactive(_) => PackageStatus::Ready,
                };

                if let Err(e) = transition_mgr.transition_to(status).await {
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
            transition_mgr
                .transition_to(PackageStatus::Error(err_str))
                .await
                .with_context(|| format!("Failed to set status of package {pkg_name} to error"))?;
        }
        result
    }

    /// Kicks off setup (and start, when autostart is enabled) in a background task.
    pub fn initialize(&self) {
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
    /// Returns an error if the package is not stopped or if wiping its data
    /// volume fails.
    pub async fn wipe_data(&self) -> anyhow::Result<()> {
        self.transition_mgr.wipe_data().await
    }

    /// # Errors
    ///
    /// Returns an error if stopping the container or unmounting the package
    /// filesystem fails during the upgrade transition.
    pub async fn prepare_upgrade(&self) -> anyhow::Result<()> {
        self.transition_mgr.prepare_upgrade().await?;
        // Stop status updates from the superseded package.
        self.receiver_handle.abort();
        Ok(())
    }

    /// # Errors
    ///
    /// Returns an error if stopping the container or unmounting the package
    /// filesystem fails.
    pub async fn teardown(&self) -> anyhow::Result<()> {
        // Abort first: a failing teardown below must not skip the release.
        self.receiver_handle.abort();

        let status = self.get_status().await?;
        // Nothing to tear down — these states have no mounted filesystem.
        if !matches!(
            status,
            PackageStatus::Verified | PackageStatus::Cleaned | PackageStatus::Upgrading
        ) {
            self.transition_mgr.teardown().await?;
        }
        Ok(())
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
        let metadata: &PackageMetadata = self.context.package_file().metadata();
        metadata.version()
    }

    #[must_use]
    pub fn get_metadata(&self) -> &PackageMetadata {
        self.context.package_file().metadata()
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
    pub async fn get_package_info(
        &self,
        package_location: PackageLocation,
    ) -> anyhow::Result<ssam_pkg_info::PackageInfo> {
        let package_metadata_ref: &PackageMetadata = self.context.package_file().metadata();

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
            package_location,
            quota_info,
        })
    }
}
