// Copyright 2025-2026 Hyundai Mobis Co., Ltd.
// SPDX-License-Identifier: Apache-2.0

use crate::configuration;
use crate::mount::{LoopDeviceAttacher, mount_pkgfs, unmount_pkgfs};
use libssam::ssam_package::PackageFsVerityInfo;
use libssam::ssam_package::ssam_pkg_payload::Payload;
use libssam::superblock::FsType;
use std::path::{Path, PathBuf};

use super::PackageFileInfo;

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

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::path::PathBuf;

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
        use libssam::ssam_package::ssam_pkg_payload::Payload;
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
}
