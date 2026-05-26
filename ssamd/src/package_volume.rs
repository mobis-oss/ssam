// Copyright 2025-2026 Hyundai Mobis Co., Ltd.
// SPDX-License-Identifier: Apache-2.0

mod data_directory;

pub(crate) use data_directory::{
    DataDirMetadata, DataDirectory, DataDirectoryManager, DefaultQuotaEntryBackend, Ext4Quota,
    QuotaInfo,
};

use crate::configuration;
use crate::mount::{LoopDeviceAttacher, mount_pkgfs, unmount_pkgfs};
use anyhow::Context;
use derive_more::Deref;
use libssam::ssam_package::ssam_pkg_payload::Payload;
use libssam::ssam_package::{PackageFile, PackageFsVerityInfo};
use libssam::superblock::FsType;
use rsactor::{Actor, ActorRef, message_handlers};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Debug)]
pub(crate) struct PackageFsInfo {
    pub(crate) fstype: FsType,
    pub(crate) pkgfs_offset: u64,
    pub(crate) length: u64,
    pub(crate) verity_info: PackageFsVerityInfo,
}

#[derive(Debug)]
pub(crate) struct PackageFsMetadata {
    pub(crate) path: PathBuf,
    pub(crate) mount_point: PathBuf,
    pub(crate) overlayfs_root: PathBuf,
    pub(crate) packagefs_info: PackageFsInfo,
}

pub trait PackageFileInfo {
    /// Returns the package filesystem metadata.
    ///
    /// # Errors
    ///
    /// Returns an error if the package file does not contain a valid filesystem payload.
    fn pkgfs(&self) -> anyhow::Result<libssam::ssam_package::PackageFilesystem>;
    fn get_package_name(&self) -> &str;
    fn get_container_data_dirs(&self) -> Option<&String>;
    fn get_container_storage_limit(&self) -> Option<&i64>;
}

impl PackageFileInfo for PackageFile {
    fn pkgfs(&self) -> anyhow::Result<libssam::ssam_package::PackageFilesystem> {
        self.pkgfs()
    }

    fn get_package_name(&self) -> &str {
        self.metadata().get_package_name()
    }

    fn get_container_data_dirs(&self) -> Option<&String> {
        self.metadata().get_container_data_dirs()
    }

    fn get_container_storage_limit(&self) -> Option<&i64> {
        self.metadata().get_container_storage_limit()
    }
}

impl PackageFsMetadata {
    pub(crate) fn new<F: PackageFileInfo + ?Sized>(
        path: impl AsRef<Path>,
        package_file: &F,
    ) -> anyhow::Result<Self> {
        let package_path = path.as_ref().to_path_buf();
        let pkgfs_info = package_file.pkgfs()?;
        let package_name = package_file.get_package_name();
        let mnt_root = configuration::packages_mnt_root();
        let mount_point = Path::new(mnt_root).join(package_name);

        let packages_overlayfs_root = configuration::packages_overlayfs_root();
        let overlayfs_root = if packages_overlayfs_root.is_empty() {
            PathBuf::new()
        } else {
            Path::new(packages_overlayfs_root).join(package_name)
        };

        let fstype = pkgfs_info.pkgfs_type;

        let Payload::INTERNAL((pkgfs_offset, length)) = pkgfs_info.payload else {
            return Err(anyhow::anyhow!(
                "Failed to get package filesystem for package: {package_name}"
            ));
        };

        let verity_info = pkgfs_info.verity_info;

        let packagefs_info = PackageFsInfo {
            fstype,
            pkgfs_offset,
            length,
            verity_info,
        };

        Ok(Self {
            path: package_path,
            mount_point,
            overlayfs_root,
            packagefs_info,
        })
    }
}

pub struct PackageVolumeMetadata {
    package_name: String,
    pkgfs_meta: PackageFsMetadata,
    data_meta: DataDirMetadata,
}

impl PackageVolumeMetadata {
    /// # Errors
    ///
    /// Returns an error if the package file metadata or filesystem info cannot be parsed.
    pub fn new<F: PackageFileInfo + ?Sized>(path: &Path, pkg_file: &F) -> anyhow::Result<Self> {
        let pkgfs_meta = PackageFsMetadata::new(path, pkg_file)?;
        let data_meta = DataDirMetadata::new(pkg_file)?;
        let package_name = pkg_file.get_package_name().to_owned();

        Ok(Self {
            package_name,
            pkgfs_meta,
            data_meta,
        })
    }
}

pub mod messages {
    use super::PackageVolumeMetadata;

    pub struct AcquireVolume {
        pub volume_meta: PackageVolumeMetadata,
    }

    pub(crate) struct PurgeVolume {
        pub(crate) name: String,
    }

    pub(crate) struct GetQuotaInfo {
        pub(crate) name: String,
    }
}

/// Operations that can be performed on a package filesystem.
///
/// This trait abstracts the mount/unmount interface over the concrete
/// `PackageFileSystem<T>` type, enabling `PackageVolumeInner` to hold an
/// `Arc<dyn PackageFsBackend>` so that the handle can be cheaply cloned and shared.
#[async_trait::async_trait]
pub trait PackageFsBackend: Send + Sync + std::fmt::Debug {
    /// Mount the package filesystem.
    ///
    /// # Errors
    ///
    /// Returns an error if the mount operation fails.
    async fn mount(&self) -> anyhow::Result<()>;
    /// Unmount the package filesystem.
    ///
    /// # Errors
    ///
    /// Returns an error if the unmount operation fails.
    fn unmount(&self) -> anyhow::Result<()>;
    /// Return the package name associated with this filesystem.
    fn package_name(&self) -> &str;
    /// Return the mount point path for this filesystem.
    fn mount_point(&self) -> &Path;
}

/// Strategy trait for mounting/unmounting package filesystems.
///
/// Implementations wrap the real kernel mount operations or test stubs,
/// allowing `PackageFileSystem` to be generic over the mount mechanism.
pub(crate) trait MountBackend: Send + Sync + std::fmt::Debug {
    fn mount_pkgfs<'a, T: LoopDeviceAttacher + Send + Sync>(
        &'a self,
        package_name: &'a str,
        pkgfs_meta: &'a PackageFsMetadata,
        loop_controller: &'a T,
    ) -> impl std::future::Future<Output = anyhow::Result<()>> + Send + 'a;

    fn unmount_pkgfs(&self, pkgfs_meta: &PackageFsMetadata) -> anyhow::Result<()>;
}

/// Production mount strategy that calls kernel mount operations.
#[derive(Debug, Default)]
pub(crate) struct DefaultMountBackend;

impl MountBackend for DefaultMountBackend {
    fn mount_pkgfs<'a, T: LoopDeviceAttacher + Send + Sync>(
        &'a self,
        package_name: &'a str,
        pkgfs_meta: &'a PackageFsMetadata,
        loop_controller: &'a T,
    ) -> impl std::future::Future<Output = anyhow::Result<()>> + Send + 'a {
        mount_pkgfs(package_name, pkgfs_meta, loop_controller)
    }

    fn unmount_pkgfs(&self, pkgfs_meta: &PackageFsMetadata) -> anyhow::Result<()> {
        unmount_pkgfs(pkgfs_meta)
    }
}

#[derive(Debug)]
pub(crate) struct PackageFileSystem<T: LoopDeviceAttacher, M: MountBackend = DefaultMountBackend> {
    pub(crate) package_name: String,
    pub(crate) loop_controller: T,
    pub(crate) pkgfs_meta: PackageFsMetadata,
    pub(crate) mount_strategy: M,
}

#[async_trait::async_trait]
impl<T, M> PackageFsBackend for PackageFileSystem<T, M>
where
    T: LoopDeviceAttacher + Send + Sync + std::fmt::Debug + 'static,
    M: MountBackend + 'static,
{
    async fn mount(&self) -> anyhow::Result<()> {
        self.mount_strategy
            .mount_pkgfs(self.package_name(), &self.pkgfs_meta, &self.loop_controller)
            .await
    }

    fn unmount(&self) -> anyhow::Result<()> {
        self.mount_strategy.unmount_pkgfs(&self.pkgfs_meta)
    }

    fn package_name(&self) -> &str {
        &self.package_name
    }

    fn mount_point(&self) -> &Path {
        &self.pkgfs_meta.mount_point
    }
}

#[derive(Debug)]
pub struct PackageVolumeInner {
    package_name: String,
    pkgfs: Arc<dyn PackageFsBackend>,
    data_directory: Option<DataDirectory<DefaultQuotaEntryBackend>>,
}

#[derive(Debug, Clone, Deref)]
pub struct PackageVolume {
    inner: Arc<PackageVolumeInner>,
}

impl PackageVolume {
    // DataDirectory is pub(crate) but PackageVolume::new is pub for integration tests.
    // The private_interfaces lint is suppressed intentionally here.
    #[allow(private_interfaces)]
    pub fn new(
        package_name: String,
        pkgfs: impl PackageFsBackend + 'static,
        data_directory: Option<DataDirectory<DefaultQuotaEntryBackend>>,
    ) -> Self {
        let inner = PackageVolumeInner {
            package_name,
            pkgfs: Arc::new(pkgfs),
            data_directory,
        };
        Self {
            inner: Arc::new(inner),
        }
    }

    pub(crate) fn data_directory(&self) -> Option<&DataDirectory<DefaultQuotaEntryBackend>> {
        self.inner.data_directory.as_ref()
    }

    pub(crate) fn packagefs(&self) -> Arc<dyn PackageFsBackend> {
        Arc::clone(&self.inner.pkgfs)
    }

    pub(crate) fn get_mount_point(&self) -> &Path {
        self.inner.pkgfs.mount_point()
    }

    #[must_use]
    pub fn package_name(&self) -> &str {
        &self.inner.package_name
    }
}

#[derive(Debug)]
pub struct PackageVolumeManagerActor {
    data_directory_manager: DataDirectoryManager<Ext4Quota>,
    loop_controller: crate::mount::LoopDeviceControl,
    volumes: HashMap<String, PackageVolume>,
}

impl Actor for PackageVolumeManagerActor {
    type Args = ();
    type Error = anyhow::Error;

    async fn on_start(_args: Self::Args, _actor_ref: &ActorRef<Self>) -> anyhow::Result<Self> {
        let root = PathBuf::from(configuration::packages_data_root());
        let quota = crate::mount::findmnt(&root).ok().and_then(|mount_point| {
            Ext4Quota::new(
                Path::new(&mount_point),
                crate::ext4quota::QuotaType::Project,
            )
            .inspect_err(|e| {
                log::warn!("Failed to initialize quota system: {e:#}");
            })
            .ok()
        });

        let data_directory_manager = DataDirectoryManager::new(quota, root).await?;
        let loop_controller = crate::mount::LoopDeviceControl::new();

        Ok(Self {
            data_directory_manager,
            loop_controller,
            volumes: HashMap::new(),
        })
    }
}

#[message_handlers]
impl PackageVolumeManagerActor {
    #[handler]
    // rsactor #[handler] requires async fn signature even without await
    #[allow(clippy::unused_async)]
    async fn handle_acquire_volume(
        &mut self,
        msg: messages::AcquireVolume,
        _actor_ref: &ActorRef<Self>,
    ) -> anyhow::Result<PackageVolume> {
        let volume_meta = msg.volume_meta;
        let package_name = volume_meta.package_name;

        let data_dir_meta = volume_meta.data_meta;
        let data_directory = self
            .data_directory_manager
            .create_data_directory(data_dir_meta)
            .inspect_err(|e| {
                log::warn!(
                    "Failed to create data directory for package {package_name}, \
                     running without data directory: {e:#}"
                );
            })
            .ok();

        let pkgfs_meta = volume_meta.pkgfs_meta;
        let loop_controller = self.loop_controller.clone();
        let pkgfs = PackageFileSystem {
            package_name: package_name.clone(),
            loop_controller,
            pkgfs_meta,
            mount_strategy: DefaultMountBackend,
        };
        let volume = PackageVolume::new(package_name.clone(), pkgfs, data_directory);
        if let Some(existing) = self.volumes.insert(package_name.clone(), volume.clone()) {
            log::warn!(
                "AcquireVolume: volume for '{}' already exists and is being replaced without explicit purge",
                existing.package_name()
            );
        }
        Ok(volume)
    }

    #[handler]
    // rsactor #[handler] requires async fn signature even without await
    #[allow(clippy::unused_async)]
    async fn handle_purge_volume(
        &mut self,
        msg: messages::PurgeVolume,
        _actor_ref: &ActorRef<Self>,
    ) -> anyhow::Result<()> {
        match self.volumes.remove(&msg.name) {
            Some(volume) => {
                if let Some(data_directory) = volume.data_directory() {
                    self.data_directory_manager
                        .remove_data_directory(data_directory)
                        .with_context(|| {
                            format!("Failed to remove data directory for package: {}", msg.name)
                        })?;
                }
                log::debug!("Purged volume for package: {}", msg.name);
            }
            None => {
                log::debug!(
                    "PurgeVolume: no volume found for package: {}, skipping",
                    msg.name
                );
            }
        }
        Ok(())
    }

    #[handler]
    // rsactor #[handler] requires async fn signature even without await
    #[allow(clippy::unused_async)]
    async fn handle_get_quota_info(
        &mut self,
        msg: messages::GetQuotaInfo,
        _actor_ref: &ActorRef<Self>,
    ) -> anyhow::Result<Option<u64>> {
        let limit = self.volumes.get(&msg.name).and_then(|v| {
            v.data_directory()
                .and_then(|d| d.quota_info())
                .and_then(QuotaInfo::block_limit)
        });
        Ok(limit)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::TempDir;

    pub(crate) mod mocks {
        use super::*;
        use crate::mount::{DevicePathBackend, LoopDeviceAttacher, loopdev_message};

        #[derive(Debug, Clone)]
        pub(crate) struct MockLoopDeviceControl;

        pub(crate) struct MockLoopDevice;

        impl DevicePathBackend for MockLoopDevice {
            fn path(&self) -> Option<PathBuf> {
                Some(PathBuf::from("/dev/dummy-loop-device"))
            }
        }

        #[async_trait::async_trait]
        impl LoopDeviceAttacher for MockLoopDeviceControl {
            type Device = MockLoopDevice;

            async fn attach(
                &self,
                _attach_info: loopdev_message::AttachInfo,
            ) -> anyhow::Result<Self::Device> {
                Ok(MockLoopDevice)
            }
        }

        #[derive(Debug)]
        pub(crate) struct MockPackageFsBackend {
            pub(crate) package_name: String,
            pub(crate) mount_point: PathBuf,
            pub(crate) mount_should_fail: bool,
            pub(crate) unmount_should_fail: bool,
        }

        #[async_trait::async_trait]
        impl PackageFsBackend for MockPackageFsBackend {
            async fn mount(&self) -> anyhow::Result<()> {
                if self.mount_should_fail {
                    anyhow::bail!("MockPackageFsBackend: mount failed")
                }
                Ok(())
            }

            fn unmount(&self) -> anyhow::Result<()> {
                if self.unmount_should_fail {
                    anyhow::bail!("MockPackageFsBackend: unmount failed")
                }
                Ok(())
            }

            fn package_name(&self) -> &str {
                &self.package_name
            }

            fn mount_point(&self) -> &Path {
                &self.mount_point
            }
        }

        #[derive(Debug, Default)]
        pub(crate) struct MockMountBackend;

        impl MountBackend for MockMountBackend {
            fn mount_pkgfs<'a, T: LoopDeviceAttacher + Send + Sync>(
                &'a self,
                _package_name: &'a str,
                _pkgfs_meta: &'a PackageFsMetadata,
                _loop_controller: &'a T,
            ) -> impl std::future::Future<Output = anyhow::Result<()>> + Send + 'a {
                std::future::ready(Ok(()))
            }

            fn unmount_pkgfs(&self, _pkgfs_meta: &PackageFsMetadata) -> anyhow::Result<()> {
                Ok(())
            }
        }
    }

    pub(crate) mod package_fs_metadata_test {
        use super::*;
        use libssam::ssam_package::PackageFilesystem;
        use libssam::superblock::FsType;
        use std::path::PathBuf;

        pub(crate) struct MockPackageFile {
            pub(crate) package_name: String,
            pub(crate) payload: Payload,
            pub(crate) pkgfs_type: FsType,
            pub(crate) verity_info: libssam::ssam_package::PackageFsVerityInfo,
        }

        impl MockPackageFile {
            // Must match signature of mount::unmount_pkgfs (swapped in via cfg alias):
            // callers use `?` on the return value, so Result<()> cannot be removed.
            #[allow(clippy::unnecessary_wraps)]
            pub(crate) fn pkgfs(&self) -> anyhow::Result<PackageFilesystem> {
                Ok(PackageFilesystem {
                    payload: self.payload.clone(),
                    pkgfs_type: self.pkgfs_type,
                    verity_info: libssam::ssam_package::PackageFsVerityInfo {
                        data_size: self.verity_info.data_size,
                        hash_size: self.verity_info.hash_size,
                        root_hash: self.verity_info.root_hash.clone(),
                        hash_offset: self.verity_info.hash_offset,
                    },
                })
            }

            pub(crate) fn get_package_name(&self) -> &str {
                &self.package_name
            }
        }

        impl PackageFileInfo for MockPackageFile {
            fn pkgfs(&self) -> anyhow::Result<libssam::ssam_package::PackageFilesystem> {
                self.pkgfs()
            }

            fn get_package_name(&self) -> &str {
                self.get_package_name()
            }

            fn get_container_data_dirs(&self) -> Option<&String> {
                None
            }

            fn get_container_storage_limit(&self) -> Option<&i64> {
                None
            }
        }

        pub(crate) fn create_test_ssam_package_file() -> MockPackageFile {
            crate::configuration::ensure_test_init();
            MockPackageFile {
                package_name: "test-package".to_string(),
                payload: Payload::INTERNAL((1024, 8192 + 2048)),
                pkgfs_type: FsType::Ext4,
                verity_info: libssam::ssam_package::PackageFsVerityInfo {
                    data_size: 4096,
                    hash_size: 2048,
                    root_hash: "abcd1234567890".to_string(),
                    hash_offset: 8192,
                },
            }
        }

        #[test]
        fn test_package_fs_metadata_new_success() {
            let pkg_file = create_test_ssam_package_file();
            let test_path = PathBuf::from("/test/package/path");

            let result = PackageFsMetadata::new(&test_path, &pkg_file);

            assert!(result.is_ok());
            let pkgfs_meta = result.unwrap();

            // Verify the basic fields
            assert_eq!(pkgfs_meta.path, test_path);
            let mnt_root = configuration::packages_mnt_root();
            assert_eq!(
                pkgfs_meta.mount_point,
                Path::new(mnt_root).join("test-package")
            );

            assert_eq!(format!("{}", pkgfs_meta.packagefs_info.fstype), "ext4");
            assert_eq!(pkgfs_meta.packagefs_info.pkgfs_offset, 1024);
            assert_eq!(pkgfs_meta.packagefs_info.length, 8192 + 2048);

            let verity = &pkgfs_meta.packagefs_info.verity_info;
            assert_eq!(verity.root_hash, "abcd1234567890");
            assert_eq!(verity.hash_offset, 8192);
            assert_eq!(verity.data_size, 4096);
        }

        #[test]
        fn test_package_fs_metadata_overlayfs_root_empty() {
            let metadata = create_test_ssam_package_file();
            let test_path = PathBuf::from("/test/package/path");

            // When PACKAGES_OVERLAYFS_ROOT is empty, overlayfs_root should be empty PathBuf
            let result = PackageFsMetadata::new(&test_path, &metadata);

            assert!(result.is_ok());
            let pkgfs_meta = result.unwrap();

            let packages_overlayfs_root = configuration::packages_overlayfs_root();
            if packages_overlayfs_root.is_empty() {
                assert_eq!(pkgfs_meta.overlayfs_root, PathBuf::new());
            } else {
                assert_eq!(
                    pkgfs_meta.overlayfs_root,
                    Path::new(packages_overlayfs_root).join("test-package")
                );
            }
        }

        #[test]
        fn test_package_fs_metadata_different_fs_types() {
            let test_path = PathBuf::from("/test/package/path");

            // Test with different filesystem types
            let fs_types = vec![FsType::Ext4, FsType::Erofs];

            for fs_type in fs_types {
                let mut pkg_file = create_test_ssam_package_file();
                pkg_file.pkgfs_type = fs_type;

                let result = PackageFsMetadata::new(&test_path, &pkg_file);
                assert!(result.is_ok());

                let pkgfs_meta = result.unwrap();
                // Compare using string representation
                assert_eq!(
                    format!("{}", pkgfs_meta.packagefs_info.fstype),
                    format!("{}", fs_type)
                );
            }
        }

        #[test]
        fn test_package_fs_metadata_with_different_offsets() {
            let test_path = PathBuf::from("/test/package/path");

            let mut pkg_file = create_test_ssam_package_file();
            pkg_file.payload = Payload::INTERNAL((2048, 16384 + 4096));
            pkg_file.verity_info.hash_offset = 16384;
            pkg_file.verity_info.hash_size = 4096;

            let result = PackageFsMetadata::new(&test_path, &pkg_file);
            assert!(result.is_ok());

            let pkgfs_meta = result.unwrap();
            assert_eq!(pkgfs_meta.packagefs_info.pkgfs_offset, 2048);
            assert_eq!(pkgfs_meta.packagefs_info.length, 16384 + 4096);
            assert_eq!(pkgfs_meta.packagefs_info.verity_info.hash_offset, 16384);
        }

        #[test]
        fn test_package_fs_metadata_with_empty_package_name() {
            let test_path = PathBuf::from("/test/package/path");

            let mut pkg_file = create_test_ssam_package_file();
            pkg_file.package_name = String::new();

            let result = PackageFsMetadata::new(&test_path, &pkg_file);
            assert!(result.is_ok());

            let pkgfs_meta = result.unwrap();
            let mnt_root = configuration::packages_mnt_root();
            assert_eq!(pkgfs_meta.mount_point, Path::new(mnt_root));
        }

        #[test]
        fn test_package_fs_metadata_with_special_characters_in_name() {
            let test_path = PathBuf::from("/test/package/path");

            let mut pkg_file = create_test_ssam_package_file();
            pkg_file.package_name = "test-package_v1.0.1".to_string();

            let result = PackageFsMetadata::new(&test_path, &pkg_file);
            assert!(result.is_ok());

            let pkgfs_meta = result.unwrap();
            let mnt_root = configuration::packages_mnt_root();
            assert_eq!(
                pkgfs_meta.mount_point,
                Path::new(mnt_root).join("test-package_v1.0.1")
            );
        }
    }

    pub(crate) mod package_volume_test {
        use super::*;
        use std::path::PathBuf;

        pub(crate) fn create_test_package_filesystem()
        -> PackageFileSystem<mocks::MockLoopDeviceControl, mocks::MockMountBackend> {
            let package_name = "test-package".to_string();
            let loop_controller = mocks::MockLoopDeviceControl;

            // Create test package file
            let test_pkg_file = package_fs_metadata_test::create_test_ssam_package_file();
            let test_path = PathBuf::from("/test/package/path");
            let pkgfs_meta = PackageFsMetadata::new(&test_path, &test_pkg_file).unwrap();

            PackageFileSystem {
                package_name,
                loop_controller,
                pkgfs_meta,
                mount_strategy: mocks::MockMountBackend,
            }
        }

        pub(crate) fn create_test_data_directory()
        -> (DataDirectory<DefaultQuotaEntryBackend>, TempDir) {
            let temp_dir = TempDir::new().unwrap();
            let test_path = temp_dir.path().join("test_package_data");
            let data_dirs_str = Some("/app/data:/var/log".to_string());
            let quota_info: Option<QuotaInfo<DefaultQuotaEntryBackend>> = None;

            let data_dir = DataDirectory::new(test_path, data_dirs_str, quota_info).unwrap();
            (data_dir, temp_dir)
        }

        #[test]
        fn test_package_volume_new() {
            let package_name = "test-package".to_string();
            let pkgfs = create_test_package_filesystem();
            let (data_directory, _temp_dir) = create_test_data_directory();

            let package_volume =
                PackageVolume::new(package_name.clone(), pkgfs, Some(data_directory));

            assert_eq!(package_volume.package_name(), "test-package");
            assert!(package_volume.data_directory().is_some());
        }

        #[test]
        fn test_package_volume_new_without_data_directory() {
            let package_name = "test-package-no-data".to_string();
            let pkgfs = create_test_package_filesystem();
            let data_directory = None;

            let package_volume = PackageVolume::new(package_name.clone(), pkgfs, data_directory);

            assert_eq!(package_volume.package_name(), "test-package-no-data");
            assert!(package_volume.data_directory().is_none());
        }

        #[test]
        fn test_package_volume_package_name() {
            let package_name = "my-test-package".to_string();
            let pkgfs = create_test_package_filesystem();
            let data_directory = None;

            let package_volume = PackageVolume::new(package_name, pkgfs, data_directory);

            assert_eq!(package_volume.package_name(), "my-test-package");
        }

        #[test]
        fn test_package_volume_data_directory_some() {
            let package_name = "test-package".to_string();
            let pkgfs = create_test_package_filesystem();
            let (data_directory, _temp_dir) = create_test_data_directory();

            let package_volume = PackageVolume::new(package_name, pkgfs, Some(data_directory));

            let retrieved_data_dir = package_volume.data_directory();
            assert!(retrieved_data_dir.is_some());

            // Test that we can access data directory methods
            let data_dir = retrieved_data_dir.unwrap();
            assert!(data_dir.path().exists());

            let data_dirs = data_dir.data_dirs();
            assert!(data_dirs.is_some());
            let dirs = data_dirs.unwrap();
            assert_eq!(dirs.len(), 2);
            assert_eq!(dirs[0], Path::new("/app/data"));
            assert_eq!(dirs[1], Path::new("/var/log"));
        }

        #[test]
        fn test_package_volume_data_directory_none() {
            let package_name = "test-package".to_string();
            let pkgfs = create_test_package_filesystem();
            let data_directory = None;

            let package_volume = PackageVolume::new(package_name, pkgfs, data_directory);

            assert!(package_volume.data_directory().is_none());
        }

        #[test]
        fn test_package_volume_packagefs() {
            let package_name = "test-package".to_string();
            let pkgfs = create_test_package_filesystem();
            let data_directory = None;

            let package_volume = PackageVolume::new(package_name, pkgfs, data_directory);

            let retrieved_pkgfs = package_volume.packagefs();
            assert_eq!(retrieved_pkgfs.package_name(), "test-package");
        }

        #[test]
        fn test_package_volume_clone() {
            let package_name = "test-package".to_string();
            let pkgfs = create_test_package_filesystem();
            let (data_directory, _temp_dir) = create_test_data_directory();

            let package_volume = PackageVolume::new(package_name, pkgfs, Some(data_directory));
            let cloned_volume = package_volume.clone();

            // Both should point to the same inner data
            assert_eq!(package_volume.package_name(), cloned_volume.package_name());
            assert_eq!(
                package_volume.data_directory().is_some(),
                cloned_volume.data_directory().is_some()
            );

            // Verify Arc sharing - should have same address
            assert!(Arc::ptr_eq(&package_volume.inner, &cloned_volume.inner));
        }

        #[test]
        fn test_package_volume_deref_trait() {
            let package_name = "test-package".to_string();
            let pkgfs = create_test_package_filesystem();
            let (data_directory, _temp_dir) = create_test_data_directory();

            let package_volume = PackageVolume::new(package_name, pkgfs, Some(data_directory));

            // Test that Deref trait allows access to inner fields
            assert_eq!(package_volume.package_name, "test-package");
            assert!(package_volume.data_directory.is_some());
        }

        #[test]
        fn test_package_volume_with_empty_package_name() {
            let package_name = String::new();
            let pkgfs = create_test_package_filesystem();
            let data_directory = None;

            let package_volume = PackageVolume::new(package_name, pkgfs, data_directory);

            assert_eq!(package_volume.package_name(), "");
        }

        #[test]
        fn test_package_volume_with_special_characters_in_name() {
            let package_name = "test-package_v1.0.1-beta+20241222".to_string();
            let pkgfs = create_test_package_filesystem();
            let data_directory = None;

            let package_volume = PackageVolume::new(package_name.clone(), pkgfs, data_directory);

            assert_eq!(package_volume.package_name(), &package_name);
        }
    }

    mod package_filesystem_test {
        use super::*;
        use std::path::PathBuf;

        fn create_test_package_filesystem_with_name(
            name: &str,
        ) -> PackageFileSystem<mocks::MockLoopDeviceControl, mocks::MockMountBackend> {
            let package_name = name.to_string();
            let loop_controller = mocks::MockLoopDeviceControl;

            let test_pkg_file = package_fs_metadata_test::create_test_ssam_package_file();
            let test_path = PathBuf::from("/test/package/path");
            let pkgfs_meta = PackageFsMetadata::new(&test_path, &test_pkg_file).unwrap();

            PackageFileSystem {
                package_name,
                loop_controller,
                pkgfs_meta,
                mount_strategy: mocks::MockMountBackend,
            }
        }

        #[test]
        fn test_package_filesystem_package_name() {
            let pkgfs = create_test_package_filesystem_with_name("test-filesystem");
            assert_eq!(pkgfs.package_name(), "test-filesystem");
        }

        #[test]
        fn test_package_filesystem_package_name_empty() {
            let pkgfs = create_test_package_filesystem_with_name("");
            assert_eq!(pkgfs.package_name(), "");
        }

        #[test]
        fn test_package_filesystem_package_name_special_chars() {
            let package_name = "test-pkg_v1.0.1-beta+20241222";
            let pkgfs = create_test_package_filesystem_with_name(package_name);
            assert_eq!(pkgfs.package_name(), package_name);
        }

        #[tokio::test]
        async fn test_package_filesystem_mount() {
            let pkgfs: Box<dyn PackageFsBackend> = Box::new(mocks::MockPackageFsBackend {
                package_name: "mount-test".to_string(),
                mount_point: PathBuf::from("/test/mount"),
                mount_should_fail: false,
                unmount_should_fail: false,
            });

            let result = pkgfs.mount().await;

            assert!(result.is_ok());
        }

        #[test]
        fn test_package_filesystem_unmount() {
            let pkgfs: Box<dyn PackageFsBackend> = Box::new(mocks::MockPackageFsBackend {
                package_name: "unmount-test".to_string(),
                mount_point: PathBuf::from("/test/mount"),
                mount_should_fail: false,
                unmount_should_fail: false,
            });

            let result = pkgfs.unmount();

            assert!(result.is_ok());
        }

        #[test]
        fn test_package_filesystem_metadata_access() {
            let pkgfs = create_test_package_filesystem_with_name("metadata-test");

            // Verify that the filesystem contains expected metadata
            // The actual metadata fields are private, but we can test the package name
            assert_eq!(pkgfs.package_name(), "metadata-test");

            // Verify debug output contains expected information
            let debug_str = format!("{pkgfs:?}");
            assert!(debug_str.contains("metadata-test"));
            assert!(debug_str.contains("pkgfs_meta"));
            assert!(debug_str.contains("loop_controller"));
        }
    }

    mod package_volume_manager_actor_tests {
        use super::*;
        use rsactor::spawn;
        // Note: The tests for PackageVolumeManagerActor are not cover the DataDirectory
        // creation logic directly, as it is handled by the DataDirectoryManager.
        // Instead, we focus on the actor's message handling and interaction with PackageVolume.

        // Helper function to create test package volume metadata
        fn create_test_volume_metadata(package_name: &str) -> PackageVolumeMetadata {
            let test_pkg_file = package_fs_metadata_test::create_test_ssam_package_file();
            let test_path = PathBuf::from("/test/package/path");
            let pkgfs_meta = PackageFsMetadata::new(&test_path, &test_pkg_file).unwrap();

            let data_meta = DataDirMetadata {
                package_name: package_name.to_string(),
                path: PathBuf::from("/tmp").join(package_name),
                data_dirs: Some("/app/data:/var/log".to_string()),
                storage_limit: Some(1024),
            };

            PackageVolumeMetadata {
                package_name: package_name.to_string(),
                pkgfs_meta,
                data_meta,
            }
        }

        #[tokio::test]
        async fn test_package_volume_manager_actor_creation() {
            let (actor_ref, join_handle) = spawn::<PackageVolumeManagerActor>(());

            // Verify actor was created successfully
            assert!(!format!("{:?}", actor_ref.identity()).is_empty());

            // Stop the actor gracefully
            actor_ref.stop().await.unwrap();

            // Verify actor completed successfully
            let result = join_handle.await.unwrap();
            match result {
                rsactor::ActorResult::Completed { .. } => {
                    // Expected successful completion
                }
                rsactor::ActorResult::Failed { error, .. } => {
                    panic!("Actor failed unexpectedly: {error}");
                }
            }
        }

        #[tokio::test]
        async fn test_handle_acquire_volume_success() {
            let (actor_ref, _join_handle) = spawn::<PackageVolumeManagerActor>(());

            // Create test volume metadata
            let volume_meta = create_test_volume_metadata("test-package");
            let acquire_msg = messages::AcquireVolume { volume_meta };

            // Send message and verify response
            let result = actor_ref.ask(acquire_msg).await.unwrap();
            assert!(result.is_ok());
            let volume = result.unwrap();
            assert_eq!(volume.package_name(), "test-package");

            // Stop the actor
            actor_ref.stop().await.unwrap();
        }

        #[tokio::test]
        async fn test_handle_acquire_volume_idempotent() {
            let (actor_ref, _join_handle) = spawn::<PackageVolumeManagerActor>(());

            let first = actor_ref
                .ask(messages::AcquireVolume {
                    volume_meta: create_test_volume_metadata("test-package-idempotent"),
                })
                .await
                .unwrap();
            assert!(first.is_ok(), "First AcquireVolume should succeed");

            let second = actor_ref
                .ask(messages::AcquireVolume {
                    volume_meta: create_test_volume_metadata("test-package-idempotent"),
                })
                .await
                .unwrap();
            assert!(
                second.is_ok(),
                "Second AcquireVolume for same package should succeed"
            );
            assert_eq!(second.unwrap().package_name(), "test-package-idempotent");

            actor_ref.stop().await.unwrap();
        }

        #[tokio::test]
        async fn test_handle_acquire_volume_with_different_packages() {
            let (actor_ref, _join_handle) = spawn::<PackageVolumeManagerActor>(());

            let package_names = vec!["package1", "package2", "package3"];

            // Create multiple package volumes
            for package_name in package_names {
                let volume_meta = create_test_volume_metadata(package_name);
                let acquire_msg = messages::AcquireVolume { volume_meta };

                let result = actor_ref.ask(acquire_msg).await.unwrap();
                assert!(result.is_ok());
                let volume = result.unwrap();
                assert_eq!(volume.package_name(), package_name);
            }

            // Stop the actor
            actor_ref.stop().await.unwrap();
        }

        #[tokio::test]
        async fn test_handle_purge_volume_success() {
            let (actor_ref, _join_handle) = spawn::<PackageVolumeManagerActor>(());

            // First create a package volume
            let volume_meta = create_test_volume_metadata("test-package-purge");
            let acquire_msg = messages::AcquireVolume { volume_meta };
            let result = actor_ref.ask(acquire_msg).await.unwrap();
            assert!(result.is_ok());

            // Now purge the volume
            let purge_result = actor_ref
                .ask(messages::PurgeVolume {
                    name: "test-package-purge".to_string(),
                })
                .await
                .unwrap();
            assert!(purge_result.is_ok());

            // Stop the actor
            actor_ref.stop().await.unwrap();
        }

        #[tokio::test]
        async fn test_handle_purge_volume_not_found() {
            let (actor_ref, _join_handle) = spawn::<PackageVolumeManagerActor>(());

            let purge_result = actor_ref
                .ask(messages::PurgeVolume {
                    name: "test-package-no-data-purge".to_string(),
                })
                .await
                .unwrap();

            assert!(purge_result.is_ok());

            // Stop the actor
            actor_ref.stop().await.unwrap();
        }

        #[tokio::test]
        async fn test_acquire_volume_with_empty_package_name() {
            let (actor_ref, _join_handle) = spawn::<PackageVolumeManagerActor>(());

            // Create metadata with empty package name
            let volume_meta = create_test_volume_metadata("");
            let acquire_msg = messages::AcquireVolume { volume_meta };

            let result = actor_ref.ask(acquire_msg).await.unwrap();
            assert!(result.is_ok());
            let volume = result.unwrap();
            assert_eq!(volume.package_name(), "");

            // Stop the actor
            actor_ref.stop().await.unwrap();
        }

        #[tokio::test]
        async fn test_acquire_volume_with_special_characters_in_name() {
            let (actor_ref, _join_handle) = spawn::<PackageVolumeManagerActor>(());

            let special_name = "test-pkg_v1.0.1-beta+20241222";
            let volume_meta = create_test_volume_metadata(special_name);
            let acquire_msg = messages::AcquireVolume { volume_meta };

            let result = actor_ref.ask(acquire_msg).await.unwrap();
            assert!(result.is_ok());
            let volume = result.unwrap();
            assert_eq!(volume.package_name(), special_name);

            // Stop the actor
            actor_ref.stop().await.unwrap();
        }
    }
}
