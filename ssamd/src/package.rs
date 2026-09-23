// Copyright 2025-2026 Hyundai Mobis Co., Ltd.
// SPDX-License-Identifier: Apache-2.0

use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::executor::container::ContainerCommand;
use crate::executor::oci_bundle_dir::OciBundleDir;
use crate::executor::{self, ContainerRuntime, ExecuteCommand, ExecutionResult, ExecutionStatus};
use crate::network::NetworkManager;
use crate::package_volume::messages::{GetQuotaInfo, WipeVolumeData};
use crate::package_volume::{PackageFsBackend, PackageVolume, PackageVolumeManagerActor};
use crate::utils::{timeline_complete, timeline_start};

use anyhow::Context as _;
use async_trait::async_trait;
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

/// States in which wiping a package's data volume is safe: no data bind mount
/// can be live. Deny-by-default keeps cleanup from running over a live source
/// when setup or unmount failed and left an Error or Broken package mounted.
pub(crate) fn is_wipeable_status(status: &PackageStatus) -> bool {
    matches!(
        status,
        PackageStatus::Verified | PackageStatus::Ready | PackageStatus::Cleaned
    )
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
struct PackageRuntime {
    executor: executor::PackageExecutor,
}

#[derive(Debug)]
struct PackageLifecycle {
    package_name: String,
    service_info: executor::systemd::ServiceInfo,
    state_sender: mpsc::Sender<ExecutionStatus>,
    packages_cgroup: String,
    pkgfs_handle: Arc<dyn PackageFsBackend>,
    oci_bundle_dir: Arc<OciBundleDir>,
    command: Arc<dyn ExecuteCommand>,
    volume_manager_ref: ActorRef<PackageVolumeManagerActor>,
    network: Option<NetworkManager>,
    bridge_id: String,
    container_interface: String,
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

    async fn setup_bundle(&self) -> anyhow::Result<()> {
        let package_name = self.package_name.as_str();
        self.oci_bundle_dir
            .ensure()
            .with_context(|| format!("{package_name}: Failed to prepare runtime directory"))?;

        let mount_handle = Self::spawn_phase(
            package_name,
            PackagePhase::Mount,
            mount_with_retry(
                Arc::clone(&self.pkgfs_handle),
                self.oci_bundle_dir.pkgfs_path(),
            ),
        );
        let prepare_command = Arc::clone(&self.command);
        let prepare_handle =
            Self::spawn_phase(package_name, PackagePhase::PrepareCommand, async move {
                prepare_command.prepare().await
            });
        let netns_net = self.network.clone();
        let netns_bridge_id = self.bridge_id.clone();
        let netns_iface = self.container_interface.clone();
        let netns_name = package_name.to_owned();
        let netns_handle =
            Self::spawn_phase(package_name, PackagePhase::AttachNetwork, async move {
                match netns_net {
                    Some(net) => {
                        net.create_netns(&netns_name).await?;
                        net.attach(&netns_name, &netns_bridge_id, &netns_iface)
                            .await
                    }
                    None => Ok(()),
                }
            });

        // join, not try_join: a panicked phase must not let cleanup race the others.
        let (mount, prepare, netns) = tokio::join!(mount_handle, prepare_handle, netns_handle);
        [mount, prepare, netns]
            .into_iter()
            .try_for_each(|result| result.context("A bundle setup task panicked")?)
    }

    async fn teardown_bundle(&self) -> anyhow::Result<()> {
        let name = self.package_name.as_str();

        // Network release is best-effort; only a failed unmount must block removal.
        if let Some(net) = &self.network {
            if let Err(error) = net.detach(name, &self.bridge_id).await {
                log::warn!("{name}: detach during teardown failed: {error:#}");
            }
            if let Err(error) = net.destroy_netns(name).await {
                log::warn!("{name}: destroy_netns during teardown failed: {error:#}");
            }
        }

        timeline_start(name, PackagePhase::Unmount);
        let unmount = self
            .pkgfs_handle
            .unmount(&self.oci_bundle_dir.pkgfs_path())
            .with_context(|| format!("{name}: Failed to unmount package fs"));
        timeline_complete(name, PackagePhase::Unmount);
        unmount?;

        self.oci_bundle_dir
            .cleanup()
            .with_context(|| format!("{name}: Failed to cleanup OCI bundle directory"))
    }

    fn runtime(&self) -> anyhow::Result<&PackageRuntime> {
        self.runtime
            .as_ref()
            .with_context(|| format!("{}: package is not provisioned", self.package_name))
    }
}

#[async_trait]
impl LifecycleBackend for PackageLifecycle {
    fn name(&self) -> &str {
        &self.package_name
    }

    async fn provision(&mut self) -> anyhow::Result<()> {
        let package_name = self.package_name.as_str();
        let executor_handle = Self::spawn_phase(
            package_name,
            PackagePhase::InitExecutor,
            executor::PackageExecutor::new(
                package_name.to_owned(),
                Arc::clone(&self.command),
                executor::ExecutorType::Systemd(self.service_info.clone()),
                self.state_sender.clone(),
                self.packages_cgroup.clone(),
            ),
        );
        let (executor, bundle) = tokio::join!(executor_handle, self.setup_bundle());
        let result = executor
            .context("The executor init task panicked")
            .and_then(|result| result) // Replace with `Result::flatten` once the MSRV reaches 1.89.
            .and_then(|executor| {
                // Kept even if the bundle failed, so removal can retry the teardown.
                self.runtime = Some(PackageRuntime { executor });
                bundle
            });
        if let Err(error) = result {
            let package_name = self.package_name.as_str();
            if let Err(cleanup_error) = self.teardown_bundle().await {
                log::warn!("{package_name}: Provisioning cleanup failed: {cleanup_error:#}");
            }
            Err(error)
        } else {
            Ok(())
        }
    }

    async fn start(&self) -> PackageStatus {
        log::debug!("Starting package {}", self.package_name);
        timeline_start(&self.package_name, PackagePhase::Start);
        let result = match self.runtime() {
            Ok(runtime) => runtime.executor.start().await,
            Err(error) => Err(error),
        };
        timeline_complete(&self.package_name, PackagePhase::Start);
        result.map_or_else(
            |error| PackageStatus::Error(format!("{error:#}")),
            PackageStatus::from,
        )
    }

    async fn stop(&self) -> PackageStatus {
        log::debug!("Stopping package {}", self.package_name);
        timeline_start(&self.package_name, PackagePhase::Stop);
        let result = match self.runtime() {
            Ok(runtime) => runtime.executor.stop().await,
            Err(error) => Err(error),
        };
        timeline_complete(&self.package_name, PackagePhase::Stop);
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
            let name = self.package_name.as_str();
            log::warn!("{name}: executor teardown failed (continuing with unmount): {error:#}");
        }
        self.teardown_bundle().await?;
        self.runtime = None;
        Ok(())
    }

    async fn wipe_data(&self) -> anyhow::Result<()> {
        self.volume_manager_ref
            .ask(WipeVolumeData {
                name: self.package_name.clone(),
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
    context: PackageContext,
    transition_mgr: TransitionManager,
    volume_manager_ref: ActorRef<PackageVolumeManagerActor>,
    receiver_handle: tokio::task::JoinHandle<anyhow::Result<()>>,
}

impl Package {
    /// # Errors
    ///
    /// Returns an error if the package requests bridge mode while the daemon
    /// network is disabled, or if building the OCI runtime config or systemd
    /// service info fails.
    pub fn new(
        context: PackageContext,
        pkg_volume: &PackageVolume,
        volume_manager_ref: ActorRef<PackageVolumeManagerActor>,
        network: Option<NetworkManager>,
        packages_cgroup: &str,
    ) -> anyhow::Result<Self> {
        let (sender, receiver) = mpsc::channel(8);

        let pkg_name = context.get_name().to_owned();
        let package_file = context.package_file();
        let metadata = package_file.metadata();

        // Authoritative network-mode parse: resolved once here and passed to both
        // the container command and the backend so they never diverge.
        let network_mode = metadata
            .get_container_network_mode()
            .map(|m| NetworkMode::parse_mode(m))
            .transpose()
            .context("Invalid network mode in package config")?
            .unwrap_or(NetworkMode::Host);

        let container_interface = metadata
            .get_container_network_bridge_interface_name()
            .map_or_else(
                || crate::network::DEFAULT_CONTAINER_INTERFACE.to_owned(),
                std::string::ToString::to_string,
            );

        // Reject bridge mode when the daemon network is disabled rather than
        // silently downgrading to host networking, which would misrepresent the
        // isolation the package declared.
        if network_mode == NetworkMode::Bridge && network.is_none() {
            anyhow::bail!(
                "{pkg_name}: bridge network mode requested but the daemon network is disabled"
            );
        }

        // netns is needed only when the (container) command runs in bridge mode;
        // gate the network manager here so the backend stays mode-agnostic.
        let effective_network = if network_mode == NetworkMode::Bridge {
            network
        } else {
            None
        };

        // bridge_id groups packages onto a shared bridge: an explicit
        // network_name shares one, its absence gives a private bridge per package.
        let bridge_id = if network_mode == NetworkMode::Bridge {
            metadata
                .get_container_network_bridge_network_name()
                .cloned()
                .unwrap_or_else(|| pkg_name.clone())
        } else {
            pkg_name.clone()
        };

        let service_info = executor::systemd::ServiceInfo::new_from_metadata(metadata)?;
        let command_spec = ContainerCommand::spec_from_package(
            &pkg_name,
            package_file,
            pkg_volume,
            network_mode,
            packages_cgroup,
        );
        let oci_bundle_dir = Arc::new(OciBundleDir::new(
            &command_spec.oci_bundle_root,
            &command_spec.package_name,
            &command_spec.version,
        )?);
        let command: Arc<dyn ExecuteCommand> = Arc::new(ContainerCommand::new(
            pkg_name.clone(),
            ContainerRuntime::CRun,
            command_spec,
            Arc::clone(&oci_bundle_dir),
        ));

        let backend = PackageLifecycle {
            package_name: pkg_name.clone(),
            service_info,
            state_sender: sender,
            packages_cgroup: packages_cgroup.to_owned(),
            pkgfs_handle: pkg_volume.packagefs(),
            oci_bundle_dir,
            command,
            volume_manager_ref: volume_manager_ref.clone(),
            network: effective_network,
            bridge_id,
            container_interface,
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
