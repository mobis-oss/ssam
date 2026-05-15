// Copyright (c) 2026 Hyundai Mobis Co., Ltd.
// SPDX-License-Identifier: Apache-2.0

use crate::package::{Package, PackageContext};
use crate::package_volume::messages::{AcquireVolume, PurgeVolume};
use crate::package_volume::{PackageVolume, PackageVolumeManagerActor, PackageVolumeMetadata};
use anyhow::Context;
use rsactor::{Actor, ActorRef, message_handlers};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::InstallInfo;
use super::fs_ops::PackageFileBackend;

use message::InstallMode;

#[derive(Actor)]
pub(crate) struct PackageTransactionActor {
    volume_manager_ref: ActorRef<PackageVolumeManagerActor>,
    fs_ops: Arc<dyn PackageFileBackend>,
}

impl PackageTransactionActor {
    pub(crate) fn new(
        volume_manager_ref: ActorRef<PackageVolumeManagerActor>,
        fs_ops: impl PackageFileBackend + 'static,
    ) -> Self {
        Self {
            volume_manager_ref,
            fs_ops: Arc::new(fs_ops),
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

    async fn acquire_volume(
        &self,
        volume_meta: PackageVolumeMetadata,
    ) -> anyhow::Result<PackageVolume> {
        self.volume_manager_ref
            .ask(AcquireVolume { volume_meta })
            .await
            .context("Failed to communicate with PackageVolumeManager Actor")?
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
        UpgradeBundled,
        UpgradeDownloaded { installed_path: PathBuf },
    }

    pub(crate) struct RemovePackageFile {
        pub(crate) path: PathBuf,
    }

    /// Acquire volume and initialize a Package from an existing file on disk.
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
    #[allow(clippy::unused_async)]
    async fn handle_remove_package_file(
        &mut self,
        msg: message::RemovePackageFile,
        _actor_ref: &ActorRef<Self>,
    ) -> anyhow::Result<bool> {
        self.remove_file_if_exists(&msg.path)
    }

    #[handler]
    async fn handle_create_package(
        &mut self,
        msg: message::CreatePackage,
        _actor_ref: &ActorRef<Self>,
    ) -> anyhow::Result<Arc<Package>> {
        let volume_meta = PackageVolumeMetadata::new(&msg.path, &msg.package_file)
            .context("Failed to create volume metadata")?;
        let volume = self.acquire_volume(volume_meta).await?;
        let context = PackageContext::new(&msg.path, msg.package_file);
        let package = Package::new(context, &volume, self.volume_manager_ref.clone())?;
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
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        path.with_extension(format!("{ext}.old"))
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
            InstallMode::UpgradeBundled => self
                .remove_file_if_exists(dest)
                .with_context(|| format!("File rollback failed (remove {dest:?})"))
                .map(|_| ()),
            InstallMode::Fresh => Ok(()),
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
            volume_meta,
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

        if remove_data {
            if let Err(e) = self.purge_volume(&package_name).await {
                self.rollback_file_ops(&mode, &dest)
                    .inspect_err(|rb| log::error!("Rollback also failed: {rb:#}"))
                    .ok();
                return Err(e);
            }
        }

        let volume = match self.acquire_volume(volume_meta).await {
            Ok(v) => v,
            Err(e) => {
                self.rollback_file_ops(&mode, &dest)
                    .inspect_err(|rb| log::error!("Rollback also failed: {rb:#}"))
                    .ok();
                return Err(e);
            }
        };

        let context = PackageContext::new(&dest, package_file);
        match Package::new(context, &volume, self.volume_manager_ref.clone()) {
            Ok(package) => {
                if let InstallMode::UpgradeDownloaded { ref installed_path } = mode {
                    let bp = Self::backup_path_of(installed_path);
                    if let Err(e) = self.fs_ops.remove_file(&bp) {
                        log::warn!("Failed to remove backup file {bp:?}: {e:#}");
                    }
                }
                Ok(Arc::new(package))
            }
            Err(e) => {
                self.rollback_file_ops(&mode, &dest)
                    .inspect_err(|rb| log::error!("Rollback also failed: {rb:#}"))
                    .ok();
                Err(e)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::fs_ops::PackageFileBackend;
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

    fn create_test_ssam_package_file(dir: &Path) -> libssam::ssam_package::PackageFile {
        use libssam::ssam_package::{PackageFilesystem, PackageFsVerityInfo};
        use libssam::superblock::FsType;

        let config_path = dir.join("config.toml");
        let runtime_path = dir.join("runtime.json");
        let seccomp_path = dir.join("seccomp.json");
        let img_path = dir.join("pkg.img");

        std::fs::write(
            &config_path,
            r#"
[package]
name = "test-pkg"
autostart = false
version = "1.0.0"
description = "unit test package"

[container]
storage_limit = 100

[container.security]
seccomp = false
mac = false

[service]
service_type = "simple"
"#,
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

        std::fs::write(&img_path, b"dummy").expect("write img");

        let pkgfs = PackageFilesystem::from_source(
            &img_path,
            FsType::Ext4,
            PackageFsVerityInfo {
                data_size: 4096,
                hash_size: 2048,
                root_hash: "abcd1234".to_string(),
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
        use crate::package_volume::tests::package_fs_metadata_test::create_test_ssam_package_file as create_test_volume_pkg;

        let package_file = create_test_ssam_package_file(dir);
        let dest = PathBuf::from("/tmp/dest.ssam");
        let mock_pkg = create_test_volume_pkg();
        let volume_meta =
            PackageVolumeMetadata::new(&dest, &mock_pkg).expect("test volume metadata");
        InstallInfo {
            package_name: name.to_string(),
            package_file,
            path: PathBuf::from("/tmp/src.ssam"),
            dest,
            volume_meta,
        }
    }

    fn spawn_actor(fs_ops: impl PackageFileBackend + 'static) -> ActorRef<PackageTransactionActor> {
        crate::configuration::ensure_test_init();
        let (vol_ref, _) = rsactor::spawn::<PackageVolumeManagerActor>(());
        let actor = PackageTransactionActor::new(vol_ref, fs_ops);
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
    async fn test_copy_failure_rolls_back_non_bundled_backup() {
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
    async fn test_copy_failure_removes_dest_for_bundled_backup() {
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
                mode: InstallMode::UpgradeBundled,
            })
            .await
            .expect("actor communication");

        assert!(result.is_err());

        let renames = renames.lock().unwrap();
        assert!(renames.is_empty(), "bundled backup should not rename");

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

    fn spawn_actor_with_dead_volume_manager(
        fs_ops: impl PackageFileBackend + 'static,
    ) -> ActorRef<PackageTransactionActor> {
        crate::configuration::ensure_test_init();
        let (vol_ref, vol_handle) = rsactor::spawn::<PackageVolumeManagerActor>(());
        vol_handle.abort();
        let actor = PackageTransactionActor::new(vol_ref, fs_ops);
        let (actor_ref, _) = rsactor::spawn::<PackageTransactionActor>(actor);
        actor_ref
    }

    #[tokio::test]
    async fn test_purge_failure_rolls_back_non_bundled_backup() {
        let ops = MockTrackingPackageFile::new();
        let renames = Arc::clone(&ops.renames);
        let actor_ref = spawn_actor_with_dead_volume_manager(ops);
        let tmp = tempfile::tempdir().expect("tempdir");
        let info = make_install_info("test-pkg", tmp.path());

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

        assert!(result.is_err(), "expected Failed when purge_volume fails");

        let renames = renames.lock().unwrap();
        assert_eq!(renames.len(), 2);
        assert_eq!(renames[0].0, installed_path);
        assert_eq!(renames[0].1, PathBuf::from("/pkg/installed.ssam.old"));
        assert_eq!(renames[1].0, PathBuf::from("/pkg/installed.ssam.old"));
        assert_eq!(renames[1].1, PathBuf::from("/pkg/installed.ssam"));
    }

    #[tokio::test]
    async fn test_acquire_failure_rolls_back_non_bundled_backup() {
        let ops = MockTrackingPackageFile::new();
        let renames = Arc::clone(&ops.renames);
        let actor_ref = spawn_actor_with_dead_volume_manager(ops);
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

        assert!(
            result.is_err(),
            "expected Failed when acquire_pkg_volume fails"
        );

        let renames = renames.lock().unwrap();
        assert_eq!(renames.len(), 2);
        assert_eq!(renames[0].0, installed_path);
        assert_eq!(renames[0].1, PathBuf::from("/pkg/installed.ssam.old"));
        assert_eq!(renames[1].0, PathBuf::from("/pkg/installed.ssam.old"));
        assert_eq!(renames[1].1, PathBuf::from("/pkg/installed.ssam"));
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
}
