// Copyright 2026 Hyundai Mobis Co., Ltd.
// SPDX-License-Identifier: Apache-2.0

use crate::configuration::Configuration;
use crate::package::Package;
use crate::package_volume::messages::PurgeVolume;
use crate::package_volume::{PackageVolumeManagerActor, data_dir_path};
use crate::utils::actor_supervisor::{IgnoreOnFailure, SupervisedActor};
use anyhow::Context;
use rsactor::{Actor, ActorRef, message_handlers};
use std::convert::Infallible;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::network::NetworkManager;

use super::InstallInfo;

use message::InstallMode;

pub(crate) trait PackageFileBackend: Send + Sync {
    fn copy(&self, from: &Path, to: &Path) -> anyhow::Result<u64>;
    fn rename(&self, from: &Path, to: &Path) -> anyhow::Result<()>;
    fn remove_file(&self, path: &Path) -> anyhow::Result<()>;
}

pub(crate) struct DefaultPackageFileBackend;

impl PackageFileBackend for DefaultPackageFileBackend {
    // Safety: Debug format ({:?}) for paths prevents log injection via special characters.
    #[allow(clippy::unnecessary_debug_formatting)]
    fn copy(&self, from: &Path, to: &Path) -> anyhow::Result<u64> {
        let dest_dir = to
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .ok_or_else(|| anyhow::anyhow!("Destination has no parent directory: {to:?}"))?;

        // new_in: same directory as dest so rename stays on one filesystem (atomic).
        // If copy fails, NamedTempFile drops and auto-deletes the partial file.
        let tmp = tempfile::NamedTempFile::new_in(dest_dir)
            .map_err(|e| anyhow::anyhow!("Cannot create temp file in {dest_dir:?}: {e}"))?;

        let bytes = std::fs::copy(from, tmp.path())
            .map_err(|e| anyhow::anyhow!("Cannot copy {from:?} to {:?}: {e}", tmp.path()))?;

        // persist: rename temp -> dest atomically; dest is never partially written.
        tmp.persist(to)
            .map_err(|e| anyhow::anyhow!("Cannot rename temp file to {to:?}: {e}"))?;

        Ok(bytes)
    }

    fn rename(&self, from: &Path, to: &Path) -> anyhow::Result<()> {
        std::fs::rename(from, to).map_err(Into::into)
    }

    fn remove_file(&self, path: &Path) -> anyhow::Result<()> {
        std::fs::remove_file(path).map_err(Into::into)
    }
}

pub(crate) struct PackageTransactionActor {
    config: Arc<Configuration>,
    volume_manager_ref: ActorRef<PackageVolumeManagerActor>,
    fs_ops: Arc<dyn PackageFileBackend>,
    network: Option<NetworkManager>,
}

impl Actor for PackageTransactionActor {
    type Args = Self;
    type Error = Infallible;
    type IdleEvent = ();

    fn on_start(
        args: Self::Args,
        _actor_ref: &ActorRef<Self>,
    ) -> impl Future<Output = Result<Self, Self::Error>> {
        std::future::ready(Ok(args))
    }
}

impl PackageTransactionActor {
    pub(crate) fn new(
        config: Arc<Configuration>,
        volume_manager_ref: ActorRef<PackageVolumeManagerActor>,
        fs_ops: impl PackageFileBackend + 'static,
        network: Option<NetworkManager>,
    ) -> Self {
        Self {
            config,
            volume_manager_ref,
            fs_ops: Arc::new(fs_ops),
            network,
        }
    }

    // Safety: Debug format ({:?}) for paths prevents log injection via special characters.
    #[allow(clippy::unnecessary_debug_formatting)]
    async fn copy_to_pkg_dir(&self, src: &Path, dest: &Path) -> anyhow::Result<u64> {
        let fs_ops = Arc::clone(&self.fs_ops);
        let src = src.to_path_buf();
        let dest = dest.to_path_buf();
        tokio::task::spawn_blocking(move || {
            fs_ops
                .copy(&src, &dest)
                .with_context(|| format!("Failed to copy package file from {src:?} to {dest:?}"))
        })
        .await
        .context("fs_ops::copy task panicked")?
    }

    async fn purge_volume(&self, package_name: &str) -> anyhow::Result<()> {
        self.volume_manager_ref
            .ask(PurgeVolume {
                name: package_name.to_owned(),
            })
            .await
            .context("Failed to communicate with PackageVolumeManager Actor")?
    }
}

impl SupervisedActor for PackageTransactionActor {
    type FailurePolicy = IgnoreOnFailure;
}

pub(crate) mod message {
    use super::super::InstallInfo;
    use libssam::ssam_package::PackageFile;
    use std::path::PathBuf;

    pub(crate) struct Install {
        pub(crate) info: InstallInfo,
        pub(crate) remove_data: bool,
        pub(crate) mode: InstallMode,
    }

    pub(crate) enum InstallMode {
        Fresh,
        UpgradeBuiltin,
        UpgradeDownloaded { installed_path: PathBuf },
    }

    pub(crate) struct RemovePackageFile {
        pub(crate) path: PathBuf,
    }

    /// Initialize a Package from an existing file on disk.
    pub(crate) struct CreatePackage {
        pub(crate) path: PathBuf,
        pub(crate) package_file: PackageFile,
    }

    pub(crate) struct PurgePackageVolume {
        pub(crate) name: String,
    }
}

#[message_handlers]
impl PackageTransactionActor {
    #[handler]
    async fn handle_install(
        &mut self,
        msg: message::Install,
        _actor_ref: &ActorRef<Self>,
    ) -> anyhow::Result<Arc<Package>> {
        self.execute_install_inner(msg.info, msg.remove_data, msg.mode)
            .await
    }

    #[handler]
    // rsactor handlers require async signatures.
    #[allow(clippy::unused_async, clippy::unused_async_trait_impl)]
    async fn handle_remove_package_file(
        &mut self,
        msg: message::RemovePackageFile,
        _actor_ref: &ActorRef<Self>,
    ) -> anyhow::Result<bool> {
        self.remove_file_if_exists(&msg.path)
    }

    #[handler]
    // rsactor handlers require async signatures.
    #[allow(clippy::unused_async, clippy::unused_async_trait_impl)]
    async fn handle_create_package(
        &mut self,
        msg: message::CreatePackage,
        _actor_ref: &ActorRef<Self>,
    ) -> anyhow::Result<Arc<Package>> {
        let package = Package::new(
            &msg.path,
            msg.package_file,
            self.volume_manager_ref.clone(),
            self.network.clone(),
            self.config.clone(),
        )?;
        Ok(Arc::new(package))
    }

    #[handler]
    async fn handle_purge_package_volume(
        &mut self,
        msg: message::PurgePackageVolume,
        _actor_ref: &ActorRef<Self>,
    ) -> anyhow::Result<()> {
        self.purge_volume(&msg.name).await
    }
}

impl PackageTransactionActor {
    fn backup_path_of(path: &Path) -> PathBuf {
        path.to_path_buf().with_added_extension("old")
    }

    /// Removes a file, treating `NotFound` as success. Returns `Ok(true)` if
    /// removed, `Ok(false)` if already absent. Propagates other I/O errors.
    fn remove_file_if_exists(&self, path: &Path) -> anyhow::Result<bool> {
        match self.fs_ops.remove_file(path) {
            Ok(()) => Ok(true),
            Err(e)
                if e.downcast_ref::<std::io::Error>()
                    .is_some_and(|io| io.kind() == std::io::ErrorKind::NotFound) =>
            {
                Ok(false)
            }
            Err(e) => Err(e),
        }
    }

    // Safety: Debug format ({:?}) for paths prevents log injection via special characters.
    #[allow(clippy::unnecessary_debug_formatting)]
    fn backup_file(&self, path: &Path) -> anyhow::Result<()> {
        let backup_path = Self::backup_path_of(path);
        self.fs_ops
            .rename(path, &backup_path)
            .with_context(|| format!("Failed to backup package file: {path:?}"))
    }

    // Safety: Debug format ({:?}) for paths prevents log injection via special characters.
    #[allow(clippy::unnecessary_debug_formatting)]
    fn rollback_file_ops(&self, mode: &InstallMode, dest: &Path) -> anyhow::Result<()> {
        match mode {
            InstallMode::UpgradeDownloaded { installed_path } => {
                let bp = Self::backup_path_of(installed_path);
                self.fs_ops.rename(&bp, installed_path).with_context(|| {
                    format!("File rollback failed (rename {bp:?} → {installed_path:?})")
                })
            }
            InstallMode::UpgradeBuiltin | InstallMode::Fresh => self
                .remove_file_if_exists(dest)
                .with_context(|| format!("File rollback failed (remove {dest:?})"))
                .map(|_| ()),
        }
    }

    /// Renames the data directory to a ".old" backup for upgrade rollback.
    /// Returns the backup path, or None if the directory does not exist.
    fn backup_data_dir(data_dir_path: &Path) -> anyhow::Result<Option<PathBuf>> {
        // Append the suffix without replacing the existing extension.
        let old = data_dir_path.to_path_buf().with_added_extension("old");
        // Clear a stale ".old"; rename onto a non-empty dir fails with ENOTEMPTY.
        std::fs::remove_dir_all(&old).ok();
        match std::fs::rename(data_dir_path, &old) {
            Ok(()) => Ok(Some(old)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).context("Failed to back up data directory for rollback"),
        }
    }

    // Safety: Debug format ({:?}) for paths prevents log injection via special characters.
    #[allow(clippy::unnecessary_debug_formatting)]
    async fn execute_install_inner(
        &self,
        install_info: InstallInfo,
        remove_data: bool,
        mode: InstallMode,
    ) -> anyhow::Result<Arc<Package>> {
        let InstallInfo {
            package_name,
            package_file,
            path,
            dest,
        } = install_info;

        if let InstallMode::UpgradeDownloaded { ref installed_path } = mode {
            self.backup_file(installed_path)?;
        }

        if let Err(e) = self.copy_to_pkg_dir(&path, &dest).await {
            self.rollback_file_ops(&mode, &dest)
                .inspect_err(|rb| log::error!("Rollback also failed: {rb:#}"))
                .ok();
            return Err(e);
        }

        // Back up the data dir before Package::new so a failure past
        // this destructive point can restore it.
        let data_dir_path = data_dir_path(self.config.packages_data_root(), &package_name);
        let data_backup_path = (remove_data
            && matches!(
                mode,
                InstallMode::UpgradeBuiltin | InstallMode::UpgradeDownloaded { .. }
            ))
        .then(|| {
            Self::backup_data_dir(&data_dir_path).inspect_err(|_| {
                self.rollback_file_ops(&mode, &dest)
                    .inspect_err(|rb| log::error!("Rollback also failed: {rb:#}"))
                    .ok();
            })
        })
        .transpose()?
        .flatten();

        match Package::new(
            &dest,
            package_file,
            self.volume_manager_ref.clone(),
            self.network.clone(),
            self.config.clone(),
        ) {
            Ok(package) => {
                if let InstallMode::UpgradeDownloaded { ref installed_path } = mode {
                    let bp = Self::backup_path_of(installed_path);
                    if let Err(e) = self.fs_ops.remove_file(&bp) {
                        log::warn!("Failed to remove backup file {bp:?}: {e:#}");
                    }
                }
                if let Some(ref backup_path) = data_backup_path
                    && let Err(e) = std::fs::remove_dir_all(backup_path)
                {
                    log::warn!("Failed to remove old data directory {backup_path:?}: {e:#}");
                }
                Ok(Arc::new(package))
            }
            Err(e) => {
                self.rollback_file_ops(&mode, &dest)
                    .inspect_err(|rb| log::error!("File rollback also failed: {rb:#}"))
                    .ok();
                if let Some(ref backup_path) = data_backup_path {
                    std::fs::remove_dir_all(&data_dir_path).ok();
                    if let Err(restore_err) = std::fs::rename(backup_path, &data_dir_path) {
                        log::error!("Failed to restore old data directory: {restore_err:#}");
                    }
                }
                Err(e)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::PackageFileBackend;
    use super::message::{Install, InstallMode};
    use super::*;
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    struct MockFailingPackageFile;

    impl PackageFileBackend for MockFailingPackageFile {
        fn copy(&self, _from: &Path, _to: &Path) -> anyhow::Result<u64> {
            Err(anyhow::anyhow!("simulated copy failure"))
        }
        fn rename(&self, _from: &Path, _to: &Path) -> anyhow::Result<()> {
            Ok(())
        }
        fn remove_file(&self, _path: &Path) -> anyhow::Result<()> {
            Ok(())
        }
    }

    struct MockSucceedingPackageFile;

    impl PackageFileBackend for MockSucceedingPackageFile {
        fn copy(&self, _from: &Path, _to: &Path) -> anyhow::Result<u64> {
            Ok(0)
        }
        fn rename(&self, _from: &Path, _to: &Path) -> anyhow::Result<()> {
            Ok(())
        }
        fn remove_file(&self, _path: &Path) -> anyhow::Result<()> {
            Ok(())
        }
    }

    fn create_test_ssam_package_file(
        dir: &Path,
        storage_limit: i64,
    ) -> libssam::ssam_package::PackageFile {
        use libssam::ssam_package::{PackageFilesystem, PackageFsVerityInfo};
        use libssam::superblock::FsType;

        let config_path = dir.join("config.toml");
        let runtime_path = dir.join("runtime.json");
        let seccomp_path = dir.join("seccomp.json");

        std::fs::write(
            &config_path,
            format!(
                r#"
[package]
name = "test-pkg"
autostart = false
version = "1.0.0"
description = "unit test package"

[container]
storage_limit = {storage_limit}

[container.security]
seccomp = false
mac = false

[container.network]

[service]
service_type = "simple"
"#
            ),
        )
        .expect("write config");

        std::fs::write(
            &runtime_path,
            r#"{
"ociVersion": "1.0.0",
"process": {"terminal": false, "user": {"uid": 0, "gid": 0}, "args": ["sh"], "env": ["PATH=/usr/bin"], "cwd": "/"},
"hostname": "test"
}"#,
        )
        .expect("write runtime config");

        std::fs::write(
            &seccomp_path,
            r#"{"defaultAction": "SCMP_ACT_ERRNO", "architectures": ["SCMP_ARCH_X86_64"], "syscalls": []}"#,
        )
        .expect("write seccomp");

        let pkgfs = PackageFilesystem::new(
            0,
            6144,
            FsType::Ext4,
            PackageFsVerityInfo {
                data_size: 4096,
                hash_size: 2048,
                table_params: "1 4096 4096 100 2 sha256 abcd1234 deadbeef".to_string(),
                hash_offset: 8192,
            },
        );

        libssam::ssam_package::PackageFile::from_source(
            config_path,
            runtime_path,
            seccomp_path,
            &pkgfs,
        )
        .expect("create test PackageFile")
    }

    fn make_install_info(name: &str, dir: &Path) -> InstallInfo {
        let package_file = create_test_ssam_package_file(dir, 100);
        let dest = PathBuf::from("/tmp/dest.ssam");
        InstallInfo {
            package_name: name.to_string(),
            package_file,
            path: PathBuf::from("/tmp/src.ssam"),
            dest,
        }
    }

    fn tempdir_configuration(root: &Path) -> Arc<Configuration> {
        crate::configuration::tests::test_configuration_with_dirs(
            &root.join("builtin"),
            &root.join("downloaded"),
            &root.join("data"),
        )
    }

    fn spawn_actor(fs_ops: impl PackageFileBackend + 'static) -> ActorRef<PackageTransactionActor> {
        spawn_actor_with_config(fs_ops, crate::configuration::tests::test_configuration())
    }

    fn spawn_actor_with_config(
        fs_ops: impl PackageFileBackend + 'static,
        config: Arc<Configuration>,
    ) -> ActorRef<PackageTransactionActor> {
        let (vol_ref, _) =
            rsactor::spawn::<PackageVolumeManagerActor>(PathBuf::from(config.packages_data_root()));
        let actor = PackageTransactionActor::new(config, vol_ref, fs_ops, None);
        let (actor_ref, _) = rsactor::spawn::<PackageTransactionActor>(actor);
        actor_ref
    }

    #[tokio::test]
    async fn test_copy_failure_returns_failed() {
        let actor_ref = spawn_actor(MockFailingPackageFile);
        let tmp = tempfile::tempdir().expect("tempdir");
        let info = make_install_info("test-pkg", tmp.path());

        let result = actor_ref
            .ask(Install {
                info,
                remove_data: false,
                mode: InstallMode::Fresh,
            })
            .await
            .expect("actor communication");

        assert!(result.is_err(), "expected Failed on copy error");
    }

    #[tokio::test]
    async fn test_invalid_volume_metadata_rejects_install() {
        let ops = MockTrackingPackageFile::new();
        let removals = Arc::clone(&ops.removals);
        let actor_ref = spawn_actor(ops);
        let tmp = tempfile::tempdir().expect("tempdir");
        let info = InstallInfo {
            package_file: create_test_ssam_package_file(tmp.path(), -1),
            ..make_install_info("test-pkg", tmp.path())
        };

        let result = actor_ref
            .ask(Install {
                info,
                remove_data: false,
                mode: InstallMode::Fresh,
            })
            .await
            .expect("actor communication");

        assert!(result.is_err(), "negative storage_limit must fail install");
        let removals = removals.lock().unwrap();
        assert_eq!(*removals, [PathBuf::from("/tmp/dest.ssam")]);
    }

    type CallLog = Arc<Mutex<Vec<(PathBuf, PathBuf)>>>;
    type RemoveLog = Arc<Mutex<Vec<PathBuf>>>;

    #[derive(Clone)]
    struct MockTrackingPackageFile {
        copy_fails: bool,
        rename_fails: bool,
        renames: CallLog,
        removals: RemoveLog,
    }

    impl MockTrackingPackageFile {
        fn new() -> Self {
            Self {
                copy_fails: false,
                rename_fails: false,
                renames: Arc::new(Mutex::new(Vec::new())),
                removals: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn with_copy_fails(mut self) -> Self {
            self.copy_fails = true;
            self
        }

        fn with_rename_fails(mut self) -> Self {
            self.rename_fails = true;
            self
        }
    }

    impl PackageFileBackend for MockTrackingPackageFile {
        fn copy(&self, _from: &Path, _to: &Path) -> anyhow::Result<u64> {
            if self.copy_fails {
                Err(anyhow::anyhow!("simulated copy failure"))
            } else {
                Ok(0)
            }
        }
        fn rename(&self, from: &Path, to: &Path) -> anyhow::Result<()> {
            if self.rename_fails {
                return Err(anyhow::anyhow!("simulated rename failure"));
            }
            self.renames
                .lock()
                .unwrap()
                .push((from.to_path_buf(), to.to_path_buf()));
            Ok(())
        }
        fn remove_file(&self, path: &Path) -> anyhow::Result<()> {
            self.removals.lock().unwrap().push(path.to_path_buf());
            Ok(())
        }
    }

    #[tokio::test]
    async fn test_copy_failure_rolls_back_non_builtin_backup() {
        let ops = MockTrackingPackageFile::new().with_copy_fails();
        let renames = Arc::clone(&ops.renames);
        let actor_ref = spawn_actor(ops);
        let tmp = tempfile::tempdir().expect("tempdir");
        let info = make_install_info("test-pkg", tmp.path());

        let installed_path = PathBuf::from("/pkg/installed.ssam");
        let result = actor_ref
            .ask(Install {
                info,
                remove_data: false,
                mode: InstallMode::UpgradeDownloaded {
                    installed_path: installed_path.clone(),
                },
            })
            .await
            .expect("actor communication");

        assert!(result.is_err());

        let renames = renames.lock().unwrap();
        assert_eq!(renames.len(), 2);
        assert_eq!(renames[0].0, installed_path);
        assert_eq!(renames[0].1, PathBuf::from("/pkg/installed.ssam.old"));
        assert_eq!(renames[1].0, PathBuf::from("/pkg/installed.ssam.old"));
        assert_eq!(renames[1].1, PathBuf::from("/pkg/installed.ssam"));
    }

    #[tokio::test]
    async fn test_copy_failure_removes_dest_for_builtin_backup() {
        let ops = MockTrackingPackageFile::new().with_copy_fails();
        let removals = Arc::clone(&ops.removals);
        let renames = Arc::clone(&ops.renames);
        let actor_ref = spawn_actor(ops);
        let tmp = tempfile::tempdir().expect("tempdir");
        let info = make_install_info("test-pkg", tmp.path());

        let result = actor_ref
            .ask(Install {
                info,
                remove_data: false,
                mode: InstallMode::UpgradeBuiltin,
            })
            .await
            .expect("actor communication");

        assert!(result.is_err());

        let renames = renames.lock().unwrap();
        assert!(renames.is_empty(), "builtin backup should not rename");

        let removals = removals.lock().unwrap();
        assert_eq!(removals.len(), 1);
        assert_eq!(removals[0], PathBuf::from("/tmp/dest.ssam"));
    }

    #[tokio::test]
    async fn test_rename_failure_returns_failed_without_copy() {
        let ops = MockTrackingPackageFile::new().with_rename_fails();
        let renames = Arc::clone(&ops.renames);
        let removals = Arc::clone(&ops.removals);
        let actor_ref = spawn_actor(ops);
        let tmp = tempfile::tempdir().expect("tempdir");
        let info = make_install_info("test-pkg", tmp.path());

        let result = actor_ref
            .ask(Install {
                info,
                remove_data: false,
                mode: InstallMode::UpgradeDownloaded {
                    installed_path: PathBuf::from("/pkg/installed.ssam"),
                },
            })
            .await
            .expect("actor communication");

        assert!(result.is_err(), "expected Failed when backup rename fails");

        let renames = renames.lock().unwrap();
        assert!(renames.is_empty());

        let removals = removals.lock().unwrap();
        assert!(removals.is_empty());
    }

    #[tokio::test]
    async fn test_remove_package_file_success() {
        let actor_ref = spawn_actor(MockSucceedingPackageFile);

        let result = actor_ref
            .ask(message::RemovePackageFile {
                path: PathBuf::from("/pkg/test.ssam"),
            })
            .await
            .expect("actor communication");

        assert!(result.expect("should succeed"));
    }

    struct MockNotFoundRemove;

    impl PackageFileBackend for MockNotFoundRemove {
        fn copy(&self, _from: &Path, _to: &Path) -> anyhow::Result<u64> {
            Ok(0)
        }
        fn rename(&self, _from: &Path, _to: &Path) -> anyhow::Result<()> {
            Ok(())
        }
        fn remove_file(&self, _path: &Path) -> anyhow::Result<()> {
            Err(std::io::Error::new(std::io::ErrorKind::NotFound, "not found").into())
        }
    }

    #[tokio::test]
    async fn test_remove_package_file_not_found_returns_false() {
        let actor_ref = spawn_actor(MockNotFoundRemove);

        let result = actor_ref
            .ask(message::RemovePackageFile {
                path: PathBuf::from("/pkg/absent.ssam"),
            })
            .await
            .expect("actor communication");

        assert!(!result.expect("NotFound should be Ok(false)"));
    }

    struct MockPermissionDeniedRemove;

    impl PackageFileBackend for MockPermissionDeniedRemove {
        fn copy(&self, _from: &Path, _to: &Path) -> anyhow::Result<u64> {
            Ok(0)
        }
        fn rename(&self, _from: &Path, _to: &Path) -> anyhow::Result<()> {
            Ok(())
        }
        fn remove_file(&self, _path: &Path) -> anyhow::Result<()> {
            Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied").into())
        }
    }

    #[tokio::test]
    async fn test_remove_package_file_other_error_propagates() {
        let actor_ref = spawn_actor(MockPermissionDeniedRemove);

        let result = actor_ref
            .ask(message::RemovePackageFile {
                path: PathBuf::from("/pkg/locked.ssam"),
            })
            .await
            .expect("actor communication");

        assert!(result.is_err(), "non-NotFound errors should propagate");
    }

    #[tokio::test]
    async fn test_success_removes_backup_file() {
        let ops = MockTrackingPackageFile::new();
        let renames = Arc::clone(&ops.renames);
        let removals = Arc::clone(&ops.removals);
        let tmp = tempfile::tempdir().expect("tempdir");
        let actor_ref = spawn_actor_with_config(ops, tempdir_configuration(tmp.path()));
        let info = make_install_info("test-pkg", tmp.path());
        let data_dir = tmp.path().join("data").join("test-pkg");
        std::fs::create_dir_all(&data_dir).expect("create data dir");

        let installed_path = PathBuf::from("/pkg/installed.ssam");
        let result = actor_ref
            .ask(Install {
                info,
                remove_data: true,
                mode: InstallMode::UpgradeDownloaded {
                    installed_path: installed_path.clone(),
                },
            })
            .await
            .expect("actor communication");

        assert!(result.is_ok(), "expected success");

        let renames = renames.lock().unwrap();
        // Only the backup rename is fs_ops-tracked; the data-dir rename is direct std::fs.
        assert_eq!(renames.len(), 1);
        assert_eq!(renames[0].0, installed_path);
        assert_eq!(renames[0].1, PathBuf::from("/pkg/installed.ssam.old"));

        let removals = removals.lock().unwrap();
        assert_eq!(removals.len(), 1);
        assert_eq!(removals[0], PathBuf::from("/pkg/installed.ssam.old"));

        assert!(!data_dir.exists(), "remove_data must drop the old data dir");
        assert!(
            !data_dir.with_extension("old").exists(),
            "the data dir backup must be removed on success"
        );
    }

    #[tokio::test]
    async fn test_failed_upgrade_restores_package_and_data() {
        let ops = MockTrackingPackageFile::new();
        let renames = Arc::clone(&ops.renames);
        let tmp = tempfile::tempdir().expect("tempdir");
        let actor_ref = spawn_actor_with_config(ops, tempdir_configuration(tmp.path()));
        let info = InstallInfo {
            package_file: create_test_ssam_package_file(tmp.path(), -1),
            ..make_install_info("test-pkg", tmp.path())
        };
        let data_dir = tmp.path().join("data").join("test-pkg");
        let user_data = data_dir.join("user-data");
        std::fs::create_dir_all(&data_dir).expect("create data dir");
        std::fs::write(&user_data, b"keep").expect("write user data");

        let installed_path = PathBuf::from("/pkg/installed.ssam");
        let result = actor_ref
            .ask(Install {
                info,
                remove_data: true,
                mode: InstallMode::UpgradeDownloaded {
                    installed_path: installed_path.clone(),
                },
            })
            .await
            .expect("actor communication");

        assert!(
            result.is_err(),
            "negative storage_limit must fail the upgrade"
        );
        let backup_path = PathBuf::from("/pkg/installed.ssam.old");
        assert_eq!(
            *renames.lock().unwrap(),
            [
                (installed_path.clone(), backup_path.clone()),
                (backup_path, installed_path),
            ]
        );
        assert!(user_data.exists(), "old data must be restored");
    }
}
