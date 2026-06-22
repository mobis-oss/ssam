// Copyright 2025-2026 Hyundai Mobis Co., Ltd.
// SPDX-License-Identifier: Apache-2.0

pub mod apparmor;
pub mod configuration;
#[cfg(feature = "dm-verity")]
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
use remocon_server_impl::RemoconImpl;

use std::{fs, net::Ipv4Addr, os::unix::fs::DirBuilderExt, path::Path, sync::Arc, time::Instant};
use tokio::sync::oneshot;

use package_manager::PackageManagerService;
use utils::actor_supervisor::spawn_with;

fn package_cgroup_exists() -> bool {
    let packages_cgroup = configuration::packages_cgroup();
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
    const DEFAULT_RPC_BIND_IP_ADDR: Ipv4Addr = Ipv4Addr::LOCALHOST;

    async fn run() -> anyhow::Result<()> {
        // Initialize configuration with default path
        let config_path = configuration::default_config_path();
        configuration::init(&config_path);

        let bundled_dir_str = configuration::bundled_packages_dir();
        let downloaded_dir_str = configuration::downloaded_packages_dir();
        Self::validate_package_dirs(bundled_dir_str, downloaded_dir_str)?;

        Self::setup_runtime_environ()?;

        if !package_cgroup_exists() {
            let packages_cgroup = configuration::packages_cgroup();
            anyhow::bail!(
                "Cgroup {packages_cgroup} does not exist. Check your build configuration."
            );
        }

        let actor =
            package_manager::PackageManagerActor::from_config(bundled_dir_str, downloaded_dir_str)
                .await?;

        let actor_ref = spawn_with::<package_manager::PackageManagerActor>(actor);

        // Refer to include/systemd/sd-daemon.h or man sd_notify(3) for detail
        sd_notify::notify(&[sd_notify::NotifyState::Ready])?;

        log::debug!("Successfully started.");
        let package_manager: Arc<dyn PackageManagerService> =
            Arc::new(package_manager::PackageManager::new(actor_ref));
        let remocon_service = RemoconImpl::new(Arc::clone(&package_manager));
        let server = libssam::remocon::remocon_server::RemoconServer::new(remocon_service);

        let ipaddr = configuration::rpc_bind_ip().map_or_else(
            || {
                log::debug!("No bind IP address configured, defaulting to localhost");
                Self::DEFAULT_RPC_BIND_IP_ADDR
            },
            |ip| {
                ip.parse::<std::net::Ipv4Addr>().unwrap_or_else(|e| {
                    log::error!("Invalid bind IP address '{ip}': {e}, defaulting to localhost");
                    Self::DEFAULT_RPC_BIND_IP_ADDR
                })
            },
        );
        let addr =
            std::net::SocketAddr::new(std::net::IpAddr::V4(ipaddr), libssam::remocon::CONTROL_PORT);

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

        log::debug!("Now client can send control messages");
        if let Err(err) = tonic::transport::Server::builder()
            .add_service(server)
            .serve_with_shutdown(addr, shutdown_receiver.map(|_| ()))
            .await
        {
            log::error!("Server error: {err:?}");
        }

        log::trace!("Terminated.");
        Ok(())
    }

    fn validate_package_dirs(
        bundled_dir_str: &str,
        downloaded_dir_str: &str,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            !bundled_dir_str.is_empty(),
            "bundled_packages_dir must not be empty"
        );
        anyhow::ensure!(
            !downloaded_dir_str.is_empty(),
            "downloaded_packages_dir must not be empty"
        );
        anyhow::ensure!(
            Path::new(bundled_dir_str).is_dir(),
            "Bundled packages directory does not exist: {bundled_dir_str}"
        );
        Ok(())
    }

    fn setup_runtime_environ() -> anyhow::Result<()> {
        let public_key_path = configuration::public_key_file_path();
        anyhow::ensure!(
            Path::new(public_key_path).is_file(),
            "Public key file not found: {public_key_path}"
        );

        let downloaded_dir = configuration::downloaded_packages_dir();
        fs::DirBuilder::new()
            .mode(0o755)
            .recursive(true)
            .create(downloaded_dir)
            .with_context(|| {
                format!("Failed to create downloaded packages directory '{downloaded_dir}'")
            })?;

        let packages_data_root = configuration::packages_data_root();
        fs::DirBuilder::new()
            .mode(0o755)
            .recursive(true)
            .create(packages_data_root)
            .with_context(|| {
                format!("Failed to create package data directory '{packages_data_root}'")
            })?;

        let packages_mnt_root = configuration::packages_mnt_root();
        fs::DirBuilder::new()
            .mode(0o755)
            .recursive(true)
            .create(packages_mnt_root)
            .with_context(|| {
                format!("Failed to create package mount root directory '{packages_mnt_root}'")
            })?;

        Ok(())
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
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn validate_rejects_empty_bundled_dir() {
        let result = Daemon::validate_package_dirs("", "/some/path");
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("bundled_packages_dir must not be empty")
        );
    }

    #[test]
    fn validate_rejects_empty_downloaded_dir() {
        let result = Daemon::validate_package_dirs("/some/path", "");
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("downloaded_packages_dir must not be empty")
        );
    }

    #[test]
    fn validate_rejects_nonexistent_bundled_dir() {
        let tmp = TempDir::new().unwrap();
        let bundled = tmp.path().join("no_such_dir");
        let downloaded = tmp.path().join("downloaded");

        let result =
            Daemon::validate_package_dirs(bundled.to_str().unwrap(), downloaded.to_str().unwrap());
        assert!(
            result.is_err(),
            "nonexistent bundled_dir should be rejected"
        );
    }
}
