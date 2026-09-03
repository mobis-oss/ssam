// Copyright 2025-2026 Hyundai Mobis Co., Ltd.
// SPDX-License-Identifier: Apache-2.0

pub mod apparmor;
pub mod configuration;
mod dm;
mod executor;
pub mod ext4quota;
pub(crate) mod mount;
pub mod network;
pub mod package;
pub mod package_manager;
pub mod package_volume;
mod remocon_server_impl;
pub(crate) mod utils;

use anyhow::Context;
use cgroups_rs::fs::{cgroup::Cgroup, hierarchies};
use futures_util::FutureExt;

use once_cell::sync::Lazy;
use remocon_server_impl::{RemoconImpl, validate_api_version};

use std::{fs, os::unix::fs::DirBuilderExt, path::Path, sync::Arc, time::Instant};
use tokio::sync::oneshot;
use tokio_stream::wrappers::UnixListenerStream;
use tonic::{Request, Status, service::InterceptorLayer, transport::server::UdsConnectInfo};

use package_manager::PackageManagerService;
use utils::actor_supervisor::spawn_with;

fn package_cgroup_exists(packages_cgroup: &str) -> bool {
    packages_cgroup.is_empty() || Cgroup::load(hierarchies::auto(), packages_cgroup).exists()
}

static GLOBAL_TIMELINE_INSTANT: Lazy<Instant> = Lazy::new(Instant::now);
static SSAMD_UPTIME: Lazy<std::time::Duration> = Lazy::new(|| {
    rustix::time::clock_gettime(rustix::time::ClockId::Monotonic)
        .try_into()
        .expect("Failed to get system uptime")
});

struct Daemon;

impl Daemon {
    async fn run() -> anyhow::Result<()> {
        let config_path = configuration::default_config_path();
        let config = Arc::new(
            configuration::Configuration::load_from_path(&config_path)
                .context("Failed to load daemon configuration")?,
        );

        Self::setup_runtime_environ(&config)?;

        if !package_cgroup_exists(config.packages_cgroup()) {
            let packages_cgroup = config.packages_cgroup();
            anyhow::bail!(
                "Cgroup {packages_cgroup} does not exist. Check your build configuration."
            );
        }

        let actor = package_manager::PackageManagerActor::new(Arc::clone(&config)).await?;

        let actor_ref = spawn_with::<package_manager::PackageManagerActor>(actor);
        let package_manager: Arc<dyn PackageManagerService> =
            Arc::new(package_manager::PackageManager::new(actor_ref));
        let remocon_service = RemoconImpl::new(Arc::clone(&package_manager), Arc::clone(&config));
        let server = libssam::remocon::remocon_server::RemoconServer::with_interceptor(
            remocon_service,
            validate_api_version,
        );

        let listener = tokio::net::UnixListener::bind(libssam::remocon::CONTROL_SOCKET_PATH)
            .context("Failed to bind abstract RPC socket")?;
        let incoming = UnixListenerStream::new(listener);

        let mut sigterm_handler =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        let mut sigint_handler =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;

        let (shutdown_sender, shutdown_receiver) = oneshot::channel::<()>();

        let pm = Arc::clone(&package_manager);
        tokio::spawn(async move {
            tokio::select! {
                _ = sigint_handler.recv() => {
                    log::info!("Received SIGINT. Exiting...");
                },
                _ = sigterm_handler.recv() => {
                    log::info!("Received SIGTERM. Exiting...");
                }
            }
            pm.teardown().await;
            if shutdown_sender.send(()).is_err() {
                log::debug!("Shutdown receiver already dropped; server may have exited early");
            }
        });

        // Refer to include/systemd/sd-daemon.h or man sd_notify(3) for detail
        sd_notify::notify(&[sd_notify::NotifyState::Ready])?;

        log::debug!("Successfully started.");
        log::debug!("Now client can send control messages");
        tonic::transport::Server::builder()
            .layer(InterceptorLayer::new(authorize_peer))
            .add_service(server)
            .serve_with_incoming_shutdown(incoming, shutdown_receiver.map(|_| ()))
            .await
            .context("RPC server failed")?;

        log::trace!("Terminated.");
        Ok(())
    }

    fn setup_runtime_environ(config: &configuration::Configuration) -> anyhow::Result<()> {
        let public_key_path = config.public_key_file_path();
        anyhow::ensure!(
            Path::new(public_key_path).is_file(),
            "Public key file not found: {public_key_path}"
        );

        let downloaded_dir = config.downloaded_packages_dir();
        fs::DirBuilder::new()
            .mode(0o755)
            .recursive(true)
            .create(downloaded_dir)
            .with_context(|| {
                format!("Failed to create downloaded packages directory '{downloaded_dir}'")
            })?;

        let packages_data_root = config.packages_data_root();
        fs::DirBuilder::new()
            .mode(0o755)
            .recursive(true)
            .create(packages_data_root)
            .with_context(|| {
                format!("Failed to create package data directory '{packages_data_root}'")
            })?;

        let packages_mnt_root = config.packages_mnt_root();
        fs::DirBuilder::new()
            .mode(0o755)
            .recursive(true)
            .create(packages_mnt_root)
            .with_context(|| {
                format!("Failed to create package mount root directory '{packages_mnt_root}'")
            })?;

        let bundle_root = configuration::runtime_bundle_root();
        fs::DirBuilder::new()
            .mode(0o755)
            .recursive(true)
            .create(&bundle_root)
            .with_context(|| {
                format!(
                    "Failed to create runtime bundle root '{}'",
                    bundle_root.display()
                )
            })?;

        Ok(())
    }
}

fn authorize_peer(request: Request<()>) -> Result<Request<()>, Status> {
    let credentials = request
        .extensions()
        .get::<UdsConnectInfo>()
        .and_then(|info| info.peer_cred.as_ref())
        .ok_or_else(|| Status::permission_denied("peer credentials unavailable"))?;

    let daemon_egid = rustix::process::getegid().as_raw();
    if credentials.gid() == daemon_egid {
        Ok(request)
    } else {
        Err(Status::permission_denied("peer credentials rejected"))
    }
}

/// # Errors
///
/// Returns an error if the logger fails to initialize, the Tokio runtime
/// cannot be built, or the daemon encounters a fatal error during execution.
pub fn main() -> anyhow::Result<()> {
    Lazy::force(&GLOBAL_TIMELINE_INSTANT); // Force to init the global instant
    Lazy::force(&SSAMD_UPTIME); // Force to init the system uptime
    ssam_log::init(log::LevelFilter::max())?;

    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(Daemon::run())
}

#[cfg(test)]
mod tests {
    use super::authorize_peer;
    use tonic::{Code, Request};

    #[test]
    fn peer_authorization_rejects_missing_credentials() {
        let status = authorize_peer(Request::new(())).expect_err("credentials must be required");
        assert_eq!(status.code(), Code::PermissionDenied);
    }

    #[tokio::test]
    async fn abstract_socket_bind_rejects_collision() {
        let path = format!("\0ssamd-test-{}", std::process::id());
        let listener = tokio::net::UnixListener::bind(&path).expect("first bind should succeed");
        assert!(tokio::net::UnixListener::bind(&path).is_err());
        drop(listener);
    }
}
