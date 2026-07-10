// Copyright 2025-2026 Hyundai Mobis Co., Ltd.
// SPDX-License-Identifier: Apache-2.0

use crate::network::NetworkManager;
use crate::package::{Package, PackageStatus};
use crate::package_manager::message::{
    GetAllPackageInfo, GetPackage, GetPackageInfo, GetPackageNames, GetPackagesStatus,
    InstallPackage, RemovePackage, StartPackage, StopPackage, TeardownPackageManager,
};
use crate::package_manager::parser::PackageParseResult;
use crate::package_manager::store::{PackageStore, PackageStoreBackend};
use crate::package_volume::{PackageVolumeManagerActor, PackageVolumeMetadata};
use crate::utils::actor_supervisor::{ExitOnFailure, SupervisedActor, spawn_with};
use anyhow::Context;
use libssam::ssam_package::PackageFile;
use libssam::ssam_package::ssam_pkg_info::{BrokenPackageInfo, BrokenReason, PackageInfoResult};
use libssam::ssam_package::ssam_pkg_metadata::PackageMetadata;
use rsactor::{Actor, ActorRef, message_handlers};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;

pub mod parser;
mod pkg_file;
mod store;

#[async_trait]
pub(crate) trait PackageManagerService: Send + Sync {
    async fn get_package_names(&self) -> Vec<String>;
    async fn get_packages_status(&self) -> anyhow::Result<Vec<(String, PackageStatus)>>;
    async fn install_package(
        &self,
        path: PathBuf,
        force: bool,
        remove_data: bool,
    ) -> anyhow::Result<PackageMetadata>;
    async fn remove_package(&self, package_name: &str, purge_volume: bool) -> anyhow::Result<()>;
    async fn start_package(&self, package_name: &str) -> anyhow::Result<()>;
    async fn stop_package(&self, package_name: &str) -> anyhow::Result<()>;
    async fn get_package_info(&self, package_name: &str) -> anyhow::Result<PackageInfoResult>;
    async fn get_all_package_info(&self) -> anyhow::Result<Vec<PackageInfoResult>>;
    async fn teardown(&self);
}

pub(crate) struct PackageManager {
    actor_ref: ActorRef<PackageManagerActor>,
}

impl PackageManager {
    pub(crate) fn new(actor_ref: ActorRef<PackageManagerActor>) -> Self {
        Self { actor_ref }
    }
}

#[async_trait]
impl PackageManagerService for PackageManager {
    async fn get_package_names(&self) -> Vec<String> {
        self.actor_ref
            .ask(GetPackageNames)
            .await
            .expect("PackageManagerActor crashed; daemon cannot function without it")
    }

    async fn get_packages_status(&self) -> anyhow::Result<Vec<(String, PackageStatus)>> {
        self.actor_ref
            .ask(GetPackagesStatus)
            .await
            .expect("PackageManagerActor crashed; daemon cannot function without it")
    }

    async fn install_package(
        &self,
        path: PathBuf,
        force: bool,
        remove_data: bool,
    ) -> anyhow::Result<PackageMetadata> {
        self.actor_ref
            .ask(InstallPackage {
                path,
                force,
                remove_data,
            })
            .await
            .expect("PackageManagerActor crashed; daemon cannot function without it")
    }

    async fn remove_package(&self, package_name: &str, purge_volume: bool) -> anyhow::Result<()> {
        self.actor_ref
            .ask(RemovePackage {
                package_name: package_name.to_owned(),
                purge_volume,
            })
            .await
            .expect("PackageManagerActor crashed; daemon cannot function without it")
    }

    async fn start_package(&self, package_name: &str) -> anyhow::Result<()> {
        self.actor_ref
            .ask(StartPackage {
                package_name: package_name.to_owned(),
            })
            .await
            .expect("PackageManagerActor crashed; daemon cannot function without it")
    }

    async fn stop_package(&self, package_name: &str) -> anyhow::Result<()> {
        self.actor_ref
            .ask(StopPackage {
                package_name: package_name.to_owned(),
            })
            .await
            .expect("PackageManagerActor crashed; daemon cannot function without it")
    }

    async fn get_package_info(&self, package_name: &str) -> anyhow::Result<PackageInfoResult> {
        self.actor_ref
            .ask(GetPackageInfo {
                package_name: package_name.to_owned(),
            })
            .await
            .expect("PackageManagerActor crashed; daemon cannot function without it")
    }

    async fn get_all_package_info(&self) -> anyhow::Result<Vec<PackageInfoResult>> {
        self.actor_ref
            .ask(GetAllPackageInfo)
            .await
            .expect("PackageManagerActor crashed; daemon cannot function without it")
    }

    async fn teardown(&self) {
        self.actor_ref
            .ask(TeardownPackageManager)
            .await
            .expect("PackageManagerActor crashed; daemon cannot function without it");
    }
}

use pkg_file::PackageTransactionActor;
use pkg_file::message::InstallMode;
use store::HashMapPackageStore;

use pkg_file::DefaultPackageFileBackend;

#[derive(Actor)]
pub struct PackageManagerActor {
    bundled_dir: PathBuf,
    downloaded_dir: PathBuf,
    package_store: PackageStore<HashMapPackageStore<Package>>,
    transaction_actor: ActorRef<PackageTransactionActor>,
}

impl SupervisedActor for PackageManagerActor {
    type FailurePolicy = ExitOnFailure;
}

struct InstallInfo {
    package_name: String,
    package_file: PackageFile,
    path: PathBuf,
    dest: PathBuf,
    volume_meta: PackageVolumeMetadata,
}

impl PackageManagerActor {
    /// Build the manager from daemon configuration, creating the bridge
    /// `NetworkManager` only when `[network] bridge_enabled` is set.
    ///
    /// # Errors
    ///
    /// Returns an error if the network manager fails to initialize or if
    /// [`Self::new`] fails.
    pub async fn from_config(
        bundled_dir_str: &str,
        downloaded_dir_str: &str,
    ) -> anyhow::Result<Self> {
        let network = crate::configuration::network_config()
            .filter(|c| c.bridge_enabled)
            .map(NetworkManager::new)
            .transpose()
            .context("Failed to initialize NetworkManager from [network] config")?;
        Self::new(bundled_dir_str, downloaded_dir_str, network).await
    }

    /// # Errors
    ///
    /// Returns an error if the directory paths cannot be canonicalized, if both
    /// directories resolve to the same path, or if loading packages fails.
    // Safety: Debug format ({:?}) for paths prevents log injection via special characters.
    #[allow(clippy::unnecessary_debug_formatting)]
    pub async fn new(
        bundled_dir_str: &str,
        downloaded_dir_str: &str,
        network: Option<NetworkManager>,
    ) -> anyhow::Result<Self> {
        // Canonical paths ensure that is_bundled_package() performs reliable
        // path comparisons even when the configuration contains symlinks.
        let bundled_dir = Path::new(bundled_dir_str)
            .canonicalize()
            .with_context(|| format!("Failed to canonicalize bundled_dir: {bundled_dir_str}"))?;
        let downloaded_dir = Path::new(downloaded_dir_str)
            .canonicalize()
            .with_context(|| {
                format!("Failed to canonicalize downloaded_dir: {downloaded_dir_str}")
            })?;
        anyhow::ensure!(
            bundled_dir != downloaded_dir,
            "bundled_dir ({bundled_dir:?}) and downloaded_dir ({downloaded_dir:?}) resolve to the same directory",
        );

        let package_store = PackageStore::new(HashMapPackageStore::new());

        let volume_manager_ref = spawn_with::<PackageVolumeManagerActor>(());
        let actor =
            PackageTransactionActor::new(volume_manager_ref, DefaultPackageFileBackend, network);
        let transaction_actor = spawn_with::<PackageTransactionActor>(actor);

        let pm = PackageManagerActor {
            bundled_dir,
            downloaded_dir,
            package_store,
            transaction_actor,
        };

        pm.reload_packages()
            .await
            .context("Failed to load packages")?;
        Ok(pm)
    }

    async fn create_package(
        &self,
        path: &Path,
        package_file: PackageFile,
    ) -> anyhow::Result<Arc<Package>> {
        self.transaction_actor
            .ask(pkg_file::message::CreatePackage {
                path: path.to_path_buf(),
                package_file,
            })
            .await
            .context("Failed to communicate with PackageTransactionActor")?
    }

    async fn reload_packages(&self) -> anyhow::Result<()> {
        log::trace!("PackageManagerActor::reload_packages");
        self.package_store.clear_all().await;

        let filter_parsed = |r: anyhow::Result<PackageParseResult>| {
            r.inspect_err(|e| log::warn!("Skipping package due to parse failure: {e:#}"))
                .ok()
        };

        // Pre-scan downloaded dir to identify packages that will override bundled.
        // This avoids fully loading bundled packages only to discard them in Phase 2.
        let downloaded_results = parser::parse_packages_from_dir(&self.downloaded_dir).await;
        let downloaded_parsed: Vec<_> = downloaded_results
            .into_iter()
            .filter_map(filter_parsed)
            .collect();
        // Downloaded packages always take priority over bundled — even broken ones.
        let overridden_names: HashSet<&str> = downloaded_parsed
            .iter()
            .map(|p| p.package_name.as_str())
            .collect();

        // Phase 1: load bundled packages. Record all bundled paths but skip full load
        // for packages that have a downloaded counterpart (valid or broken).
        let bundled_packages = parser::parse_packages_from_dir(&self.bundled_dir)
            .await
            .into_iter()
            .filter_map(filter_parsed);
        for parsed in bundled_packages {
            self.package_store
                .insert_bundled_path(parsed.package_name.clone(), parsed.path.clone())
                .await;
            if overridden_names.contains(parsed.package_name.as_str()) {
                log::debug!(
                    "Deferring bundled '{}': downloaded override exists",
                    parsed.package_name
                );
                continue;
            }
            self.load_parsed_package(parsed).await?;
        }

        // Phase 2: load downloaded packages (overrides bundled on name collision).
        for parsed in downloaded_parsed {
            let name = parsed.package_name.clone();
            self.load_parsed_package(parsed).await?;
            if self.package_store.get_bundled_path(&name).await.is_some() {
                log::info!("Downloaded package '{name}' overrides bundled version");
            }
        }

        Ok(())
    }

    async fn load_parsed_package(&self, parsed: PackageParseResult) -> anyhow::Result<()> {
        let PackageParseResult {
            package_name,
            package_file,
            path,
            ..
        } = parsed;

        match package_file {
            Ok(package_file) => {
                let package = self
                    .create_package(&path, package_file)
                    .await
                    .with_context(|| format!("Failed to create package: {package_name}"))?;

                if self
                    .package_store
                    .insert(package_name.clone(), package)
                    .await
                    .is_some()
                {
                    log::info!(
                        "Package '{package_name}' already exists, replaced with reloaded version",
                    );
                }
            }
            Err(e) => {
                log::error!("Failed to parse package {package_name}: {e:#}");
                self.package_store
                    .insert_broken(
                        package_name,
                        BrokenPackageInfo {
                            package_path: path
                                .canonicalize()
                                .unwrap_or_else(|_| path.clone())
                                .display()
                                .to_string(),
                            broken_info: BrokenReason::from(&e),
                        },
                    )
                    .await;
            }
        }

        Ok(())
    }

    async fn get_package(&self, package_name: &str) -> Option<Arc<Package>> {
        log::trace!("PackageManagerActor::get_package");
        self.package_store.get(package_name).await
    }

    fn is_valid_package_name(name: &str) -> bool {
        crate::utils::is_safe_path_segment(name)
    }

    // Safety: Debug format ({:?}) for paths prevents log injection via special characters.
    #[allow(clippy::unnecessary_debug_formatting)]
    fn ensure_external_source(&self, path: &Path) -> anyhow::Result<()> {
        let src_canon = path
            .canonicalize()
            .with_context(|| format!("Cannot resolve source path: {path:?}"))?;

        if src_canon.starts_with(&self.downloaded_dir) {
            return Err(anyhow::anyhow!(
                "Source file {:?} is inside the downloaded package directory {:?}; install from an external path",
                path,
                self.downloaded_dir
            ));
        }

        if src_canon.starts_with(&self.bundled_dir) {
            return Err(anyhow::anyhow!(
                "Source file {:?} is inside the bundled package directory {:?}; install from an external path",
                path,
                self.bundled_dir
            ));
        }

        Ok(())
    }

    // Safety: Debug format ({:?}) for paths prevents log injection via special characters.
    #[allow(clippy::unnecessary_debug_formatting)]
    fn prepare_install_package(
        &self,
        parsed_package: PackageParseResult,
        orig_pkg: Option<&Arc<Package>>,
        force: bool,
    ) -> anyhow::Result<InstallInfo> {
        let PackageParseResult {
            package_name,
            package_file,
            path,
            ..
        } = parsed_package;

        let package_file =
            package_file.with_context(|| format!("Failed to parse package from file {path:?}"))?;

        let metadata = package_file.metadata();
        let version = metadata.version();

        if let Some(orig) = &orig_pkg {
            let orig_ver = orig.get_version();
            if version <= orig_ver && !force {
                return Err(anyhow::anyhow!(
                    "A same or newer version of package '{package_name}' is already installed \
                     (installed: {orig_ver}, new: {version})",
                ));
            }
        }

        anyhow::ensure!(
            Self::is_valid_package_name(&package_name),
            "Invalid package name: {package_name}"
        );

        let packages_ext = crate::configuration::packages_ext();
        let dest_filename = format!("{package_name}.{packages_ext}");
        let dest = self.downloaded_dir.join(&dest_filename);

        let volume_meta = PackageVolumeMetadata::new(&dest, &package_file)
            .context("Failed to create volume metadata")?;

        Ok(InstallInfo {
            package_name,
            package_file,
            path,
            dest,
            volume_meta,
        })
    }

    async fn store_package(&self, package_name: String, package: Arc<Package>) {
        log::debug!("Stored package: {package_name}");
        let _ = self.package_store.insert(package_name, package).await;
    }

    // Safety: Debug format ({:?}) for paths prevents log injection via special characters.
    #[allow(clippy::unnecessary_debug_formatting)]
    async fn recover_package(&self, package_path: &Path) -> anyhow::Result<Arc<Package>> {
        let parse_result = parser::parse_package(package_path)
            .await
            .with_context(|| format!("Failed to re-parse package at {package_path:?}"))?;
        let package_file = parse_result
            .package_file
            .with_context(|| format!("Failed to re-parse package at {package_path:?}"))?;
        self.create_package(package_path, package_file)
            .await
            .with_context(|| format!("Failed to recover package from {package_path:?}"))
    }
    // Safety: Debug format ({:?}) for paths prevents log injection via special characters.
    #[allow(clippy::unnecessary_debug_formatting)]
    async fn try_recover(&self, package_name: &str, package_path: &Path) {
        match self.recover_package(package_path).await {
            Ok(restored) => {
                self.package_store
                    .insert(package_name.to_owned(), restored)
                    .await;
                log::info!("Restored package '{package_name}' from {package_path:?}");
            }
            Err(e) => {
                log::error!(
                    "Recovery failed for '{package_name}': {e:#}. \
                     Removing from store; will recover on next reload."
                );
                self.package_store.remove(package_name).await;
            }
        }
    }

    async fn do_install(
        &self,
        install_info: InstallInfo,
        remove_data: bool,
        mode: InstallMode,
    ) -> anyhow::Result<Arc<Package>> {
        self.transaction_actor
            .ask(pkg_file::message::Install {
                info: install_info,
                remove_data,
                mode,
            })
            .await
            .context("Failed to communicate with PackageTransactionActor")?
    }

    async fn remove_package_file(&self, path: &Path) -> anyhow::Result<bool> {
        self.transaction_actor
            .ask(pkg_file::message::RemovePackageFile {
                path: path.to_path_buf(),
            })
            .await
            .context("Failed to communicate with PackageTransactionActor")?
    }

    fn is_bundled_package(&self, path: &Path) -> bool {
        path.parent() == Some(self.bundled_dir.as_path())
    }

    async fn remove_package(&self, package_name: &str, purge: bool) -> anyhow::Result<()> {
        log::debug!("Removing package: {package_name}");
        if let Some(package) = self.package_store.get(package_name).await {
            let package_path = package.get_package_file_path().to_path_buf();

            anyhow::ensure!(
                !self.is_bundled_package(&package_path),
                "Cannot remove bundled package '{package_name}': system-provided"
            );

            self.teardown(&package, package_name)
                .await
                .with_context(|| {
                    format!("Failed to teardown package {package_name} before removal")
                })?;
            self.package_store.remove(package_name).await;

            if purge {
                log::debug!("Purging package volume for package: {package_name}");
                if let Err(purge_err) = self
                    .transaction_actor
                    .ask(pkg_file::message::PurgePackageVolume {
                        name: package_name.to_owned(),
                    })
                    .await
                    .context("Failed to communicate with PackageTransactionActor")
                    .and_then(|r| r)
                {
                    log::error!(
                        "Purge failed for '{package_name}': {purge_err:#}. \
                         Attempting to reload package from disk."
                    );
                    self.try_recover(package_name, &package_path).await;
                    return Err(purge_err.context(format!(
                        "Failed to purge package volume for package: {package_name}"
                    )));
                }
            }

            self.remove_package_file(&package_path)
                .await
                .context("Failed to remove package file")?;

            if let Some(bundled_path) = self.package_store.get_bundled_path(package_name).await {
                log::info!(
                    "Restoring bundled package '{package_name}' after removing downloaded override"
                );
                self.try_recover(package_name, &bundled_path).await;
                if self.package_store.get(package_name).await.is_none() {
                    log::warn!(
                        "Bundled restoration incomplete for '{package_name}'; \
                         package will be restored on next daemon reload"
                    );
                }
            }
        } else if let Some(broken_info) = self.package_store.get_broken(package_name).await {
            anyhow::ensure!(
                !self.is_bundled_package(Path::new(&broken_info.package_path)),
                "Cannot remove broken bundled package '{package_name}': system-provided"
            );
            self.package_store.remove_broken(package_name).await;
            self.remove_package_file(Path::new(&broken_info.package_path))
                .await
                .context("Failed to remove broken package file")?;

            if let Some(bundled_path) = self.package_store.get_bundled_path(package_name).await {
                log::info!(
                    "Restoring bundled package '{package_name}' \
                     after removing broken downloaded override"
                );
                self.try_recover(package_name, &bundled_path).await;
            }
        } else {
            return Err(anyhow::anyhow!("Package not found: {package_name}"));
        }

        log::debug!("Successfully removed package: {package_name}");
        Ok(())
    }

    async fn teardown(&self, package: &Package, package_name: &str) -> anyhow::Result<()> {
        package
            .teardown()
            .await
            .with_context(|| format!("Failed to teardown package {package_name}"))
    }

    async fn get_package_statuses(&self) -> anyhow::Result<Vec<(String, PackageStatus)>> {
        log::trace!("PackageManagerActor::get_package_statuses");

        let mut status_results = Vec::new();
        let packages = self.package_store.get_all().await;

        for (package_name, package) in packages {
            let status = package
                .get_status()
                .await
                .with_context(|| format!("Failed to get status of package {package_name}"))?;
            status_results.push((package_name, status));
        }

        let broken_pkgs = self.package_store.get_broken_all().await;
        for (filename, info) in broken_pkgs {
            let status = PackageStatus::Broken(info.broken_info);
            // For broken packages, we don't have a package object, so we use an empty string for the name
            status_results.push((filename, status));
        }

        Ok(status_results)
    }

    async fn teardown_all(&self) {
        let packages_map = self.package_store.get_all().await;
        for (name, p) in &packages_map {
            if let Err(e) = self.teardown(p, name).await {
                log::warn!("teardown failed for package '{name}' during daemon shutdown: {e:#}");
            }
        }
        self.package_store.clear().await;
        self.package_store.clear_broken().await;
    }
}

// PackageManagerActor Messages implementation
#[message_handlers]
impl PackageManagerActor {
    #[handler]
    async fn handle_get_package(
        &mut self,
        msg: GetPackage,
        _actor_ref: &ActorRef<Self>,
    ) -> Option<Arc<Package>> {
        self.get_package(&msg.package_name).await
    }

    #[handler]
    async fn handle_start_package(
        &mut self,
        msg: StartPackage,
        _actor_ref: &ActorRef<Self>,
    ) -> anyhow::Result<()> {
        log::trace!("PackageManagerActor::start_package");
        let package_name = msg.package_name;
        let package = self
            .get_package(&package_name)
            .await
            .ok_or_else(|| anyhow::anyhow!("Package not found: {package_name}"))?;
        package.request_start().await
    }

    #[handler]
    async fn handle_stop_package(
        &mut self,
        msg: StopPackage,
        _actor_ref: &ActorRef<Self>,
    ) -> anyhow::Result<()> {
        log::trace!("PackageManagerActor::stop_package");
        let package_name = msg.package_name;
        let package = self
            .get_package(&package_name)
            .await
            .ok_or_else(|| anyhow::anyhow!("Package not found: {package_name}"))?;
        package.request_stop().await
    }

    // Safety: Debug format ({:?}) for paths prevents log injection via special characters.
    #[allow(clippy::unnecessary_debug_formatting)]
    #[handler]
    async fn handle_install_package(
        &mut self,
        msg: InstallPackage,
        _actor_ref: &ActorRef<Self>,
    ) -> anyhow::Result<PackageMetadata> {
        log::trace!("PackageManagerActor::install_package");
        let path = msg.path;
        let force = msg.force;
        let remove_data = msg.remove_data;
        if !path.exists() {
            return Err(anyhow::anyhow!("File not found: {path:?}"))
                .context(format!("Failed to install package from file: {path:?}"));
        }
        if !parser::is_package_file(&path) {
            return Err(anyhow::anyhow!("Invalid package file: {path:?}"))
                .context(format!("Failed to install package from file: {path:?}"));
        }
        self.ensure_external_source(&path)
            .with_context(|| format!("Failed to install package from file: {path:?}"))?;

        log::info!("Parsing package file: {path:?}");
        let parsed_package = parser::parse_package(&path)
            .await
            .with_context(|| format!("Failed to parse package from file {path:?}"))?;

        let orig_pkg = self.package_store.get(&parsed_package.package_name).await;
        let prepared = self.prepare_install_package(parsed_package, orig_pkg.as_ref(), force)?;
        let package_name = prepared.package_name.clone();

        if let Some(dest_name) = prepared.dest.file_name().and_then(|f| f.to_str()) {
            self.package_store.remove_broken(dest_name).await;
        }

        if let Some(existing) = orig_pkg {
            let installed_path = existing.get_package_file_path().to_path_buf();
            let is_bundled = self.is_bundled_package(&installed_path);
            existing
                .prepare_upgrade()
                .await
                .with_context(|| format!("Cannot prepare upgrade for package({package_name})"))?;

            let mode = if is_bundled {
                InstallMode::UpgradeBundled
            } else {
                InstallMode::UpgradeDownloaded {
                    installed_path: installed_path.clone(),
                }
            };

            match self.do_install(prepared, remove_data, mode).await {
                Ok(built) => {
                    let metadata = built.get_metadata().clone();
                    self.store_package(built.get_name().to_owned(), built).await;
                    Ok(metadata)
                }
                Err(source) => {
                    self.try_recover(&package_name, &installed_path).await;
                    Err(source.context(format!("Failed to install package from file: {path:?}")))
                }
            }
        } else {
            let built = self
                .do_install(prepared, false, InstallMode::Fresh)
                .await
                .with_context(|| format!("Failed to install package from file: {path:?}"))?;
            let metadata = built.get_metadata().clone();
            self.store_package(built.get_name().to_owned(), built).await;
            Ok(metadata)
        }
    }

    #[handler]
    async fn handle_remove_package(
        &mut self,
        msg: RemovePackage,
        _actor_ref: &ActorRef<Self>,
    ) -> anyhow::Result<()> {
        log::trace!("PackageManagerActor::remove_package");
        let package_name = msg.package_name;
        let purge_volume = msg.purge_volume;
        self.remove_package(&package_name, purge_volume)
            .await
            .with_context(|| format!("Failed to remove package: {package_name}"))
    }

    #[handler]
    async fn handle_get_packages_status(
        &mut self,
        _msg: GetPackagesStatus,
        _actor_ref: &ActorRef<Self>,
    ) -> anyhow::Result<Vec<(String, PackageStatus)>> {
        log::trace!("PackageManagerActor::get_package_statuses");
        self.get_package_statuses().await
    }

    #[handler]
    async fn handle_get_package_names(
        &mut self,
        _msg: GetPackageNames,
        _actor_ref: &ActorRef<Self>,
    ) -> Vec<String> {
        self.package_store.get_all().await.into_keys().collect()
    }

    #[handler]
    async fn handle_teardown(&mut self, _msg: TeardownPackageManager, _actor_ref: &ActorRef<Self>) {
        log::trace!("PackageManagerActor::teardown");
        self.teardown_all().await;
    }

    #[handler]
    async fn handle_get_package_info(
        &mut self,
        msg: GetPackageInfo,
        _actor_ref: &ActorRef<Self>,
    ) -> anyhow::Result<PackageInfoResult> {
        let package_name = msg.package_name;

        if let Some(package) = self.get_package(&package_name).await {
            package
                .get_package_info()
                .await
                .with_context(|| format!("Failed to get package info of package: {package_name}"))
                .map(PackageInfoResult::Normal)
        } else {
            self.package_store
                .get_broken(&package_name)
                .await
                .map(|info| PackageInfoResult::Broken {
                    filename: package_name.clone(),
                    info,
                })
                .ok_or_else(|| anyhow::anyhow!("Package not found: {package_name}"))
        }
    }

    #[handler]
    async fn handle_get_all_package_info(
        &mut self,
        _msg: GetAllPackageInfo,
        _actor_ref: &ActorRef<Self>,
    ) -> anyhow::Result<Vec<PackageInfoResult>> {
        log::trace!("PackageManagerActor::get_all_package_info");

        let mut results = Vec::new();
        let packages = self.package_store.get_all().await;

        for (name, package) in packages {
            let info = package
                .get_package_info()
                .await
                .with_context(|| format!("Failed to get package info of package: {name}"))?;
            results.push(PackageInfoResult::Normal(info));
        }

        let broken_pkgs = self.package_store.get_broken_all().await;
        for (filename, info) in broken_pkgs {
            results.push(PackageInfoResult::Broken { filename, info });
        }

        Ok(results)
    }
}

// Actor messages
pub mod message {

    use std::path::PathBuf;

    pub struct GetPackage {
        pub package_name: String,
    }

    pub struct InstallPackage {
        pub path: PathBuf,
        pub force: bool,
        pub remove_data: bool,
    }

    pub struct RemovePackage {
        pub package_name: String,
        pub purge_volume: bool,
    }

    pub struct StartPackage {
        pub package_name: String,
    }

    pub struct StopPackage {
        pub package_name: String,
    }

    pub struct GetPackagesStatus;

    pub struct GetPackageNames;

    pub struct GetAllPackageInfo;

    pub struct TeardownPackageManager;
    pub struct GetPackageInfo {
        pub package_name: String,
    }
}

#[cfg(test)]
impl PackageManagerActor {
    fn new_with_fs_ops(
        bundled_dir: PathBuf,
        downloaded_dir: PathBuf,
        fs_ops: impl pkg_file::PackageFileBackend + 'static,
    ) -> Self {
        let package_store = PackageStore::new(HashMapPackageStore::new());
        let (volume_manager_ref, _) = rsactor::spawn::<PackageVolumeManagerActor>(());
        let actor = PackageTransactionActor::new(volume_manager_ref, fs_ops, None);
        let (transaction_actor, _) = rsactor::spawn::<PackageTransactionActor>(actor);
        PackageManagerActor {
            bundled_dir,
            downloaded_dir,
            package_store,
            transaction_actor,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pkg_file::PackageFileBackend;
    use std::sync::{Arc, Mutex};

    type CallLog<T> = Arc<Mutex<Vec<T>>>;

    fn new_call_log<T>() -> CallLog<T> {
        Arc::new(Mutex::new(Vec::new()))
    }

    #[derive(Clone)]
    struct FsOpsCallTracker {
        copies: CallLog<(PathBuf, PathBuf)>,
        renames: CallLog<(PathBuf, PathBuf)>,
        removals: CallLog<PathBuf>,
    }

    impl FsOpsCallTracker {
        fn new() -> Self {
            Self {
                copies: new_call_log(),
                renames: new_call_log(),
                removals: new_call_log(),
            }
        }
    }

    struct MockPackageFileBackend {
        tracker: FsOpsCallTracker,
    }

    impl MockPackageFileBackend {
        fn new(tracker: FsOpsCallTracker) -> Self {
            Self { tracker }
        }
    }

    impl PackageFileBackend for MockPackageFileBackend {
        fn copy(&self, from: &Path, to: &Path) -> anyhow::Result<u64> {
            self.tracker
                .copies
                .lock()
                .expect("lock poisoned")
                .push((from.to_path_buf(), to.to_path_buf()));
            Ok(0)
        }

        fn rename(&self, from: &Path, to: &Path) -> anyhow::Result<()> {
            self.tracker
                .renames
                .lock()
                .expect("lock poisoned")
                .push((from.to_path_buf(), to.to_path_buf()));
            Ok(())
        }

        fn remove_file(&self, path: &Path) -> anyhow::Result<()> {
            self.tracker
                .removals
                .lock()
                .expect("lock poisoned")
                .push(path.to_path_buf());
            Ok(())
        }
    }

    fn make_pm(ops: MockPackageFileBackend) -> (PackageManagerActor, tempfile::TempDir) {
        crate::configuration::ensure_test_init();
        let tmp = tempfile::tempdir().expect("failed to create temp dir");
        (
            PackageManagerActor::new_with_fs_ops(
                PathBuf::from("/nonexistent/bundled"),
                tmp.path().to_path_buf(),
                ops,
            ),
            tmp,
        )
    }

    fn tracker_and_ops() -> (FsOpsCallTracker, MockPackageFileBackend) {
        let t = FsOpsCallTracker::new();
        let ops = MockPackageFileBackend::new(t.clone());
        (t, ops)
    }

    #[tokio::test]
    async fn test_install_rejects_source_inside_package_dir() {
        let (_tracker, ops) = tracker_and_ops();
        crate::configuration::ensure_test_init();
        let tmp = tempfile::tempdir().expect("failed to create temp dir");

        let pkg_filename = format!("test-pkg.{}", crate::configuration::packages_ext());
        let internal_source = tmp.path().join(&pkg_filename);
        std::fs::write(&internal_source, b"fake").expect("write test file");

        let pm = PackageManagerActor::new_with_fs_ops(
            PathBuf::from("/nonexistent/bundled"),
            tmp.path().to_path_buf(),
            ops,
        );
        let result = pm.ensure_external_source(&internal_source);
        assert!(result.is_err());
        let err_msg = format!("{:#}", result.unwrap_err());
        assert!(
            err_msg.contains("inside the downloaded package directory"),
            "expected source-in-pkgdir error, got: {err_msg}"
        );
    }

    #[tokio::test]
    async fn test_install_rejects_source_inside_bundled_dir() {
        let (_tracker, ops) = tracker_and_ops();
        crate::configuration::ensure_test_init();
        let tmp = tempfile::tempdir().expect("failed to create temp dir");
        let bundled = tmp.path().join("bundled");
        let downloaded = tmp.path().join("downloaded");
        std::fs::create_dir_all(&bundled).expect("create bundled dir");
        std::fs::create_dir_all(&downloaded).expect("create downloaded dir");

        let pkg_filename = format!("test-pkg.{}", crate::configuration::packages_ext());
        let internal_source = bundled.join(&pkg_filename);
        std::fs::write(&internal_source, b"fake").expect("write test file");

        let pm = PackageManagerActor::new_with_fs_ops(bundled.clone(), downloaded, ops);
        let result = pm.ensure_external_source(&internal_source);
        assert!(result.is_err());
        let err_msg = format!("{:#}", result.unwrap_err());
        assert!(
            err_msg.contains("inside the bundled package directory"),
            "expected source-in-bundled-dir error, got: {err_msg}"
        );
    }

    #[test]
    fn test_valid_package_name_accepts_simple_name() {
        assert!(PackageManagerActor::is_valid_package_name("my-package"));
        assert!(PackageManagerActor::is_valid_package_name("pkg_v2.3"));
        assert!(PackageManagerActor::is_valid_package_name("nginx"));
    }

    #[test]
    fn test_valid_package_name_rejects_traversal() {
        assert!(!PackageManagerActor::is_valid_package_name("../evil"));
        assert!(!PackageManagerActor::is_valid_package_name("../../evil"));
        assert!(!PackageManagerActor::is_valid_package_name("sub/../evil"));
    }

    #[test]
    fn test_valid_package_name_rejects_subdirectory() {
        assert!(!PackageManagerActor::is_valid_package_name("subdir/name"));
    }

    #[test]
    fn test_valid_package_name_rejects_absolute_path() {
        assert!(!PackageManagerActor::is_valid_package_name("/abs/name"));
        assert!(!PackageManagerActor::is_valid_package_name("/etc/passwd"));
    }

    #[test]
    fn test_valid_package_name_rejects_empty() {
        assert!(!PackageManagerActor::is_valid_package_name(""));
    }

    #[test]
    fn test_valid_package_name_rejects_trailing_slash() {
        assert!(!PackageManagerActor::is_valid_package_name("pkg/"));
    }

    #[tokio::test]
    async fn test_is_bundled_package_detection() {
        let (_, ops) = tracker_and_ops();
        let (pm, _tmp) = make_pm(ops);
        assert!(pm.is_bundled_package(Path::new("/nonexistent/bundled/pkg.ssam")));
        assert!(!pm.is_bundled_package(Path::new("/tmp/downloaded/pkg.ssam")));
        assert!(!pm.is_bundled_package(Path::new("/nonexistent/bundled-extra/pkg.ssam")));
    }

    #[tokio::test]
    async fn test_remove_broken_bundled_package_is_rejected() {
        let (_tracker, ops) = tracker_and_ops();
        let (pm, _tmp) = make_pm(ops);

        pm.package_store
            .insert_broken(
                "broken-bundled-pkg".to_string(),
                BrokenPackageInfo {
                    package_path: "/nonexistent/bundled/broken-bundled-pkg.ssam".to_string(),
                    broken_info: libssam::ssam_package::ssam_pkg_info::BrokenReason::new(
                        "parse failure",
                        "mock broken package for testing",
                    ),
                },
            )
            .await;

        let result = pm.remove_package("broken-bundled-pkg", true).await;
        assert!(
            result.is_err(),
            "should reject removal of broken bundled package"
        );
        let err_msg = format!("{:#}", result.unwrap_err());
        assert!(
            err_msg.contains("Cannot remove broken bundled package"),
            "expected bundled rejection error, got: {err_msg}"
        );
    }

    #[tokio::test]
    async fn test_remove_broken_downloaded_package_succeeds() {
        let (tracker, ops) = tracker_and_ops();
        let (pm, _tmp) = make_pm(ops);

        let pkg_path = "/tmp/downloaded/broken-dl.ssam";
        pm.package_store
            .insert_broken(
                "broken-dl".to_string(),
                BrokenPackageInfo {
                    package_path: pkg_path.to_string(),
                    broken_info: BrokenReason::new("parse failure", "corrupted package"),
                },
            )
            .await;

        let result = pm.remove_package("broken-dl", true).await;
        assert!(
            result.is_ok(),
            "removing broken downloaded package should succeed"
        );

        assert!(
            pm.package_store.get_broken("broken-dl").await.is_none(),
            "broken entry should be cleared from store"
        );

        let removals = tracker.removals.lock().unwrap();
        assert_eq!(removals.len(), 1);
        assert_eq!(removals[0], PathBuf::from(pkg_path));
    }

    #[tokio::test]
    async fn test_remove_broken_downloaded_restores_bundled() {
        let (tracker, ops) = tracker_and_ops();
        let (pm, _tmp) = make_pm(ops);

        let broken_dl_path = "/tmp/downloaded/shared-pkg.ssam";
        let bundled_path = PathBuf::from("/nonexistent/bundled/shared-pkg.ssam");

        pm.package_store
            .insert_bundled_path("shared-pkg".to_string(), bundled_path.clone())
            .await;

        pm.package_store
            .insert_broken(
                "shared-pkg".to_string(),
                BrokenPackageInfo {
                    package_path: broken_dl_path.to_string(),
                    broken_info: BrokenReason::new("parse failure", "corrupted download"),
                },
            )
            .await;

        let result = pm.remove_package("shared-pkg", true).await;
        assert!(result.is_ok(), "removing broken downloaded should succeed");

        assert!(
            pm.package_store.get_broken("shared-pkg").await.is_none(),
            "broken entry should be cleared"
        );

        {
            let removals = tracker.removals.lock().unwrap();
            assert_eq!(
                removals.len(),
                1,
                "should remove the broken downloaded file"
            );
            assert_eq!(removals[0], PathBuf::from(broken_dl_path));
        }

        assert!(
            pm.package_store
                .get_bundled_path("shared-pkg")
                .await
                .is_some(),
            "bundled_path should persist after removing the downloaded override"
        );
    }

    mod adapter_tests {
        use super::*;
        use tempfile::TempDir;

        async fn setup_package_manager() -> (PackageManager, TempDir, TempDir) {
            let bundled_dir = TempDir::new().expect("bundled temp dir");
            let downloaded_dir = TempDir::new().expect("downloaded temp dir");

            let actor = PackageManagerActor::new(
                bundled_dir.path().to_str().unwrap(),
                downloaded_dir.path().to_str().unwrap(),
                None,
            )
            .await
            .expect("PackageManagerActor creation should succeed with empty dirs");

            let (actor_ref, _) = rsactor::spawn::<PackageManagerActor>(actor);
            let package_manager = PackageManager::new(actor_ref);

            (package_manager, bundled_dir, downloaded_dir)
        }

        #[tokio::test]
        async fn test_get_package_names_empty() {
            let (pm, _b, _d) = setup_package_manager().await;
            let names = pm.get_package_names().await;
            assert!(names.is_empty());
        }

        #[tokio::test]
        async fn test_get_packages_status_empty() {
            let (pm, _b, _d) = setup_package_manager().await;
            let result = pm.get_packages_status().await;
            assert!(result.is_ok());
            assert!(result.unwrap().is_empty());
        }

        #[tokio::test]
        async fn test_get_all_package_info_empty() {
            let (pm, _b, _d) = setup_package_manager().await;
            let result = pm.get_all_package_info().await;
            assert!(result.is_ok());
            assert!(result.unwrap().is_empty());
        }

        #[tokio::test]
        async fn test_get_package_info_not_found() {
            let (pm, _b, _d) = setup_package_manager().await;
            let result = pm.get_package_info("nonexistent").await;
            assert!(result.is_err());
        }

        #[tokio::test]
        async fn test_start_package_not_found() {
            let (pm, _b, _d) = setup_package_manager().await;
            let result = pm.start_package("nonexistent").await;
            assert!(result.is_err());
        }

        #[tokio::test]
        async fn test_stop_package_not_found() {
            let (pm, _b, _d) = setup_package_manager().await;
            let result = pm.stop_package("nonexistent").await;
            assert!(result.is_err());
        }

        #[tokio::test]
        async fn test_remove_package_not_found() {
            let (pm, _b, _d) = setup_package_manager().await;
            let result = pm.remove_package("nonexistent", true).await;
            assert!(result.is_err());
        }

        #[tokio::test]
        async fn test_install_package_invalid_path() {
            let (pm, _b, _d) = setup_package_manager().await;
            let result = pm
                .install_package(PathBuf::from("/nonexistent/path.ssam"), false, false)
                .await;
            assert!(result.is_err());
        }
    }
}
