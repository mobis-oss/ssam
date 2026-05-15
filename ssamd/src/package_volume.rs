// Copyright 2025-2026 Hyundai Mobis Co., Ltd.
// SPDX-License-Identifier: Apache-2.0

use crate::configuration;
use crate::ext4quota::Ext4QuotaEntry;
use crate::mount::{LoopDeviceAttacher, mount_pkgfs, unmount_pkgfs};
use crate::utils::quota_utils;
use anyhow::Context;
use derive_more::Deref;
use libssam::ssam_package::ssam_pkg_payload::Payload;
use libssam::ssam_package::{PackageFile, PackageFsVerityInfo};
use libssam::superblock::FsType;
use rsactor::{Actor, ActorRef, message_handlers};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

fn parse_data_dirs(data_dirs_str: Option<String>) -> Option<Vec<PathBuf>> {
    data_dirs_str.and_then(|s| {
        let s_is_not_empty = !s.trim().is_empty();
        s_is_not_empty.then(|| {
            s.split(':')
                .filter_map(|x| {
                    let trimmed = x.trim();
                    (!trimmed.is_empty()).then(|| PathBuf::from(trimmed))
                })
                .collect()
        })
    })
}

pub(crate) trait QuotaEntryBackend {
    fn set_block_limits(&self, soft_limit: u64, hard_limit: u64) -> anyhow::Result<()>;
    fn set_project_quota(&self, path: &Path, enabled: bool) -> anyhow::Result<()>;
}

#[derive(Debug)]
pub(crate) struct DefaultQuotaEntryBackend {
    inner: Ext4QuotaEntry,
}

impl QuotaEntryBackend for DefaultQuotaEntryBackend {
    fn set_block_limits(&self, soft_limit: u64, hard_limit: u64) -> anyhow::Result<()> {
        self.inner
            .set_block_limits(soft_limit, hard_limit)
            .with_context(|| format!("Failed block limit for project ID: {}", self.inner.id()))
    }

    fn set_project_quota(&self, path: &Path, enabled: bool) -> anyhow::Result<()> {
        let id = if enabled {
            usize::try_from(self.inner.id())
                .with_context(|| format!("Quota entry id {} is negative", self.inner.id()))?
        } else {
            0
        };
        quota_utils::set_project_quota(path, enabled, id).with_context(|| {
            format!(
                "Failed to set project quota for directory: {}",
                path.display()
            )
        })
    }
}

impl DefaultQuotaEntryBackend {
    pub(crate) fn new(quota_entry: Ext4QuotaEntry) -> Self {
        Self { inner: quota_entry }
    }
}

#[derive(Debug)]
pub(crate) struct QuotaInfo<T: QuotaEntryBackend> {
    entry: T,
    block_limit: Option<u64>,
}

impl<T: QuotaEntryBackend> QuotaInfo<T> {
    pub(crate) fn new(entry: T, block_limit: Option<u64>) -> Self {
        Self { entry, block_limit }
    }

    pub(crate) fn block_limit(&self) -> Option<u64> {
        self.block_limit
    }
}

#[derive(Debug)]
pub(crate) struct DataDirectory<T: QuotaEntryBackend> {
    path: PathBuf,
    data_dirs: Option<Vec<PathBuf>>,
    quota_info: Option<QuotaInfo<T>>,
}

impl<T: QuotaEntryBackend> DataDirectory<T> {
    pub(crate) fn new(
        path: PathBuf,
        data_dirs_str: Option<String>,
        quota_info: Option<QuotaInfo<T>>,
    ) -> anyhow::Result<Self> {
        crate::utils::make_directory(&path, true)
            .with_context(|| format!("Failed to create data directory: {}", path.display()))?;

        let data_dirs = parse_data_dirs(data_dirs_str);

        Ok(Self {
            path,
            data_dirs,
            quota_info,
        })
    }

    pub(crate) fn path(&self) -> &Path {
        self.path.as_path()
    }

    pub(crate) fn data_dirs(&self) -> Option<Vec<&Path>> {
        self.data_dirs
            .as_ref()
            .map(|dirs| dirs.iter().map(PathBuf::as_path).collect())
    }

    pub(crate) fn ensure_data_dirs(&self) -> anyhow::Result<()> {
        let data_dirs = self.data_dirs();
        let path = self.path();

        if let Some(data_dirs) = data_dirs {
            // Create the source directory for bind mount (in host data directory)
            for dir in data_dirs {
                // Remove leading "/" if present to make it relative
                let relative_dir = dir.strip_prefix("/").unwrap_or(dir);
                let src_path = path.join(relative_dir);
                crate::utils::make_directory(&src_path, true).with_context(|| {
                    format!(
                        "Failed to create bind mount source directory: {}",
                        src_path.display()
                    )
                })?;
            }
        }
        Ok(())
    }

    pub(crate) fn remove(&self) -> anyhow::Result<()> {
        std::fs::remove_dir_all(self.path())
            .with_context(|| format!("Failed to remove data directory: {}", self.path().display()))
    }

    /// Set the quota block limit of the data directory.
    ///
    /// When `enabled` is true:
    /// - Enables project inheritance of the directory
    /// - Sets the project ID from the quota info
    /// - Applies the configured block limit (or 0 if not configured)
    ///
    /// When `enabled` is false:
    /// - Disables project inheritance of the directory
    /// - Resets the project ID to 0
    /// - Sets the block limit to 0 (effectively disabling quota)
    ///
    /// # Arguments
    /// * `enabled` - Whether to enable or disable the quota block limit
    ///
    /// # Errors
    /// Returns an error if:
    /// - Setting project inheritance fails
    /// - Setting project ID fails
    /// - Setting block limits fails
    ///
    /// # Note
    /// If quota is not available on the system, this function logs a warning
    /// but does not return an error.
    pub(crate) fn set_block_limit(&self, enabled: bool) -> anyhow::Result<()> {
        if let Some(quota_info) = &self.quota_info {
            let entry = &quota_info.entry;

            entry
                .set_project_quota(self.path(), enabled)
                .with_context(|| {
                    format!(
                        "Failed to set project quota for directory: {}",
                        self.path().display()
                    )
                })?;

            let block_limit = if enabled {
                quota_info.block_limit.unwrap_or(0)
            } else {
                0
            };

            entry.set_block_limits(0, block_limit).with_context(|| {
                format!(
                    "Failed to {} block limit for directory: {}",
                    if enabled { "enable" } else { "disable" },
                    self.path().display()
                )
            })?;
        } else {
            log::warn!("Quota might be disabled for this system");
        }
        Ok(())
    }

    pub(crate) fn quota_info(&self) -> Option<&QuotaInfo<T>> {
        self.quota_info.as_ref()
    }
}

#[cfg_attr(test, derive(Default))]
pub(crate) struct DataDirMetadata {
    package_name: String,
    pub(crate) path: PathBuf,
    pub(crate) data_dirs: Option<String>,
    pub(crate) storage_limit: Option<u64>,
}

impl DataDirMetadata {
    pub(crate) fn new<F: PackageFileInfo + ?Sized>(pkg_file: &F) -> anyhow::Result<Self> {
        let package_name = pkg_file.get_package_name().to_string();
        let data_dirs = pkg_file.get_container_data_dirs().cloned();
        let storage_limit = pkg_file
            .get_container_storage_limit()
            .map(|v| u64::try_from(*v))
            .transpose()
            .context("storage_limit value is invalid")?;
        let root = configuration::packages_data_root();
        let path = PathBuf::from(root).join(&package_name);
        Ok(Self {
            package_name,
            path,
            data_dirs,
            storage_limit,
        })
    }
}

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

#[derive(Debug)]
struct QuotaProjectIdManager {
    project_map: HashMap<PathBuf, usize>,
}

impl QuotaProjectIdManager {
    async fn new<Q: Ext4QuotaBackend>(data_root: impl AsRef<Path>, quota: Option<&Q>) -> Self {
        let project_map = Self::initialize_directory_map(data_root, quota)
            .await
            .unwrap_or_default();
        Self { project_map }
    }

    async fn initialize_directory_map<Q: Ext4QuotaBackend>(
        data_root: impl AsRef<Path>,
        quota: Option<&Q>,
    ) -> anyhow::Result<HashMap<PathBuf, usize>> {
        let mut directory_project_map = HashMap::new();

        let mut read_dir = tokio::fs::read_dir(&data_root).await.with_context(|| {
            format!(
                "Failed to read data directory: {}",
                data_root.as_ref().display()
            )
        })?;

        while let Some(entry) = read_dir
            .next_entry()
            .await
            .with_context(|| "Failed to read directory entry")?
        {
            let path = entry.path();
            if path.is_dir() {
                let project_id = if let Some(q) = quota {
                    q.get_project_id_for_dir(&path)
                } else {
                    Some(directory_project_map.len() + 1)
                };
                if let Some(id) = project_id {
                    directory_project_map.insert(path, id);
                }
            }
        }

        Ok(directory_project_map)
    }

    fn next_id(&self) -> usize {
        self.project_map.values().max().copied().unwrap_or_default() + 1
    }

    fn get_project_id(&mut self, path: impl AsRef<Path>) -> usize {
        let path = path.as_ref();
        if let Some(&id) = self.project_map.get(path) {
            id
        } else {
            let new_id = self.next_id();
            self.project_map.insert(path.to_owned(), new_id);
            log::debug!(
                "Assigned new project ID {} for path: {}",
                new_id,
                path.display()
            );
            new_id
        }
    }

    fn remove_id(&mut self, path: impl AsRef<Path>) {
        let path = path.as_ref();
        if self.project_map.remove(path).is_some() {
            log::debug!("Removed project ID for path {}", path.display());
        } else {
            log::warn!("No project ID found for path {}", path.display());
        }
    }
}

#[derive(Debug)]
pub(crate) struct DataDirectoryManager<Q: Ext4QuotaBackend> {
    quota_root: Option<Q>,
    id_mgr: QuotaProjectIdManager,
}

pub(crate) trait Ext4QuotaBackend {
    type Entry: QuotaEntryBackend;
    fn enforce(&self) -> anyhow::Result<()>;
    fn entry(&self, id: i32) -> Self::Entry;
    /// Read the project ID assigned to `path` from filesystem metadata.
    /// Returns `None` if the directory has no project ID or on any error.
    fn get_project_id_for_dir(&self, path: &Path) -> Option<usize>;
}

#[derive(Debug, derive_more::Deref)]
struct Ext4Quota {
    inner: crate::ext4quota::Ext4Quota,
}

impl Ext4Quota {
    pub(crate) fn new(
        mount_point: &Path,
        quota_type: crate::ext4quota::QuotaType,
    ) -> anyhow::Result<Self> {
        let inner =
            crate::ext4quota::Ext4Quota::new(mount_point, quota_type).with_context(|| {
                format!(
                    "Failed to initialize Ext4 quota for mount point: {}",
                    mount_point.display()
                )
            })?;
        Ok(Self { inner })
    }
}

impl Ext4QuotaBackend for Ext4Quota {
    type Entry = DefaultQuotaEntryBackend;
    fn enforce(&self) -> anyhow::Result<()> {
        self.inner
            .enforce()
            .with_context(|| "Failed to enforce quota on mount point")
    }

    fn entry(&self, id: i32) -> Self::Entry {
        DefaultQuotaEntryBackend::new(self.inner.entry(id))
    }

    fn get_project_id_for_dir(&self, path: &Path) -> Option<usize> {
        quota_utils::get_projid(path).ok()
    }
}

impl<Q: Ext4QuotaBackend> DataDirectoryManager<Q> {
    async fn new(quota_root: Option<Q>, data_root: impl AsRef<Path>) -> anyhow::Result<Self> {
        let mount_point = data_root.as_ref().to_path_buf();

        if let Some(quota) = &quota_root {
            quota.enforce().with_context(|| {
                format!(
                    "Failed to enforce quota on mount point: {}",
                    mount_point.display()
                )
            })?;
        }

        let id_mgr = QuotaProjectIdManager::new(&data_root, quota_root.as_ref()).await;

        Ok(Self { quota_root, id_mgr })
    }

    fn create_data_directory(
        &mut self,
        dir_meta: DataDirMetadata,
    ) -> anyhow::Result<DataDirectory<Q::Entry>> {
        let path = dir_meta.path;
        let package_name = dir_meta.package_name;
        let data_dirs = dir_meta.data_dirs;

        let quota_info = self
            .quota_root
            .as_ref()
            .map(|quota| -> anyhow::Result<_> {
                let id = self.id_mgr.get_project_id(&path);
                let block_limit = dir_meta.storage_limit;
                // ID 0 is the ext4 root project; defaulting to it would corrupt
                // quota isolation, so propagate the error instead.
                let id_i32 = i32::try_from(id).with_context(|| {
                    format!("Project id {id} overflows i32 — this should never happen")
                })?;
                let entry = quota.entry(id_i32);
                Ok(QuotaInfo::new(entry, block_limit))
            })
            .transpose()?;

        let data_directory =
            DataDirectory::new(path, data_dirs, quota_info).with_context(|| {
                format!("Failed to create data directory for package: {package_name}")
            })?;

        data_directory.set_block_limit(true).context(format!(
            "Failed to set block limit for data directory: {}",
            data_directory.path().display()
        ))?;

        data_directory.ensure_data_dirs().with_context(|| {
            format!("Failed to create data directory for package: {package_name}")
        })?;

        Ok(data_directory)
    }

    fn remove_data_directory(&mut self, data_dir: &DataDirectory<Q::Entry>) -> anyhow::Result<()> {
        if let Err(e) = data_dir.set_block_limit(false) {
            log::warn!(
                "Failed to disable block limit for data directory {}: {e:#}",
                data_dir.path().display()
            );
        }
        self.id_mgr.remove_id(data_dir.path());
        data_dir.remove()
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
        pub(crate) struct MockQuotaEntryBackend {
            should_fail: bool,
        }

        impl QuotaEntryBackend for MockQuotaEntryBackend {
            fn set_block_limits(&self, _soft_limit: u64, _hard_limit: u64) -> anyhow::Result<()> {
                if self.should_fail {
                    Err(anyhow::anyhow!("Simulated quota operation failure"))
                } else {
                    Ok(())
                }
            }

            fn set_project_quota(&self, _path: &Path, _enabled: bool) -> anyhow::Result<()> {
                if self.should_fail {
                    Err(anyhow::anyhow!("Simulated set_project_quota failure"))
                } else {
                    Ok(())
                }
            }
        }

        impl MockQuotaEntryBackend {
            pub(crate) fn new(should_fail: bool) -> Self {
                Self { should_fail }
            }
        }

        #[derive(Debug)]
        pub(crate) struct MockExt4QuotaManager {
            pub(crate) should_fail: bool,
        }

        impl Ext4QuotaBackend for MockExt4QuotaManager {
            type Entry = tests::mocks::MockQuotaEntryBackend;

            fn enforce(&self) -> anyhow::Result<()> {
                if self.should_fail {
                    anyhow::bail!("Dummy enforce failed")
                }
                Ok(())
            }

            fn entry(&self, _id: i32) -> Self::Entry {
                tests::mocks::MockQuotaEntryBackend::new(self.should_fail)
            }

            fn get_project_id_for_dir(&self, _path: &Path) -> Option<usize> {
                Some(1)
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

    mod util_functions_test {
        use super::*;
        #[test]
        fn test_parse_data_dirs_none() {
            let res = parse_data_dirs(None);
            assert!(res.is_none());
        }

        #[test]
        fn test_parse_data_dirs_empty_string() {
            let res = parse_data_dirs(Some("   ".to_string()));
            assert!(res.is_none());
        }

        #[test]
        fn test_parse_data_dirs_single_path() {
            let res = parse_data_dirs(Some("/var/data".to_string()));
            let expected = vec![PathBuf::from("/var/data")];
            assert_eq!(res.unwrap(), expected);
        }

        #[test]
        fn test_parse_data_dirs_multiple_and_spaces() {
            let input = " /a : b :  : /c ".to_string();
            let res = parse_data_dirs(Some(input));
            let expected = vec![PathBuf::from("/a"), PathBuf::from("b"), PathBuf::from("/c")];
            assert_eq!(res.unwrap(), expected);
        }

        #[test]
        fn test_parse_data_dirs_trailing_colon() {
            let res = parse_data_dirs(Some("a:".to_string()));
            let expected = vec![PathBuf::from("a")];
            assert_eq!(res.unwrap(), expected);
        }
    }

    mod data_directory_test {
        use super::*;

        fn create_dummy_quota_info(
            block_limit: Option<u64>,
        ) -> QuotaInfo<mocks::MockQuotaEntryBackend> {
            let entry = mocks::MockQuotaEntryBackend::new(false);
            QuotaInfo::new(entry, block_limit)
        }

        #[test]
        fn test_new_creates_directory() {
            let temp_dir = TempDir::new().unwrap();
            let test_path = temp_dir.path().join("test_package");

            let data_dir =
                DataDirectory::<DefaultQuotaEntryBackend>::new(test_path.clone(), None, None)
                    .unwrap();

            assert!(test_path.exists());
            assert!(test_path.is_dir());
            assert_eq!(data_dir.path(), test_path.as_path());
        }

        #[test]
        fn test_new_with_data_dirs_string() {
            let temp_dir = TempDir::new().unwrap();
            let test_path = temp_dir.path().join("test_package");
            let data_dirs_str = Some("/app/data:/var/log".to_string());

            let data_dir = DataDirectory::<DefaultQuotaEntryBackend>::new(
                test_path.clone(),
                data_dirs_str,
                None,
            )
            .unwrap();

            assert_eq!(data_dir.path(), test_path.as_path());

            let data_dirs = data_dir.data_dirs().unwrap();
            assert_eq!(data_dirs.len(), 2);
            assert_eq!(data_dirs[0], Path::new("/app/data"));
            assert_eq!(data_dirs[1], Path::new("/var/log"));
        }

        #[test]
        fn test_new_with_quota_info() {
            let temp_dir = TempDir::new().unwrap();
            let test_path = temp_dir.path().join("test_package");
            let quota_info = Some(create_dummy_quota_info(Some(1024)));

            let data_dir = DataDirectory::new(test_path.clone(), None, quota_info).unwrap();

            assert_eq!(data_dir.path(), test_path.as_path());
            assert!(data_dir.quota_info.is_some());
        }

        #[test]
        fn test_data_dirs_none() {
            let temp_dir = TempDir::new().unwrap();
            let test_path = temp_dir.path().join("test_package");

            let data_dir =
                DataDirectory::<DefaultQuotaEntryBackend>::new(test_path, None, None).unwrap();

            assert!(data_dir.data_dirs().is_none());
        }

        #[test]
        fn test_data_dirs_some() {
            let temp_dir = TempDir::new().unwrap();
            let test_path = temp_dir.path().join("test_package");
            let data_dirs_str = Some("/app/data:/var/log:/tmp/cache".to_string());

            let data_dir =
                DataDirectory::<DefaultQuotaEntryBackend>::new(test_path, data_dirs_str, None)
                    .unwrap();

            let dirs = data_dir.data_dirs().unwrap();
            assert_eq!(dirs.len(), 3);
            assert_eq!(dirs[0], Path::new("/app/data"));
            assert_eq!(dirs[1], Path::new("/var/log"));
            assert_eq!(dirs[2], Path::new("/tmp/cache"));
        }

        #[test]
        fn test_ensure_data_dirs_none() {
            let temp_dir = TempDir::new().unwrap();
            let test_path = temp_dir.path().join("test_package");

            let data_dir =
                DataDirectory::<DefaultQuotaEntryBackend>::new(test_path, None, None).unwrap();

            // Should not fail when data_dirs is None
            assert!(data_dir.ensure_data_dirs().is_ok());
        }

        #[test]
        fn test_ensure_data_dirs_creates_subdirectories() {
            let temp_dir = TempDir::new().unwrap();
            let test_path = temp_dir.path().join("test_package");
            let data_dirs_str = Some("app/data:var/log:tmp/cache".to_string());

            let data_dir = DataDirectory::<DefaultQuotaEntryBackend>::new(
                test_path.clone(),
                data_dirs_str,
                None,
            )
            .unwrap();

            assert!(data_dir.ensure_data_dirs().is_ok());

            // Check that subdirectories were created
            assert!(test_path.join("app/data").exists());
            assert!(test_path.join("var/log").exists());
            assert!(test_path.join("tmp/cache").exists());
        }

        #[test]
        fn test_ensure_data_dirs_strips_leading_slash() {
            let temp_dir = TempDir::new().unwrap();
            let test_path = temp_dir.path().join("test_package");
            let data_dirs_str = Some("/app/data:/var/log".to_string());

            let data_dir = DataDirectory::<DefaultQuotaEntryBackend>::new(
                test_path.clone(),
                data_dirs_str,
                None,
            )
            .unwrap();

            assert!(data_dir.ensure_data_dirs().is_ok());

            // Check that subdirectories were created without leading slash
            assert!(test_path.join("app/data").exists());
            assert!(test_path.join("var/log").exists());
        }

        #[test]
        fn test_remove_deletes_directory() {
            let temp_dir = TempDir::new().unwrap();
            let test_path = temp_dir.path().join("test_package");

            let data_dir =
                DataDirectory::<DefaultQuotaEntryBackend>::new(test_path.clone(), None, None)
                    .unwrap();

            // Verify directory exists
            assert!(test_path.exists());

            // Remove the directory
            assert!(data_dir.remove().is_ok());

            // Verify directory is deleted
            assert!(!test_path.exists());
        }

        #[test]
        fn test_remove_nonexistent_directory_fails() {
            let temp_dir = TempDir::new().unwrap();
            let test_path = temp_dir.path().join("nonexistent");

            // Create DataDirectory without actually creating the directory
            let data_dir: DataDirectory<mocks::MockQuotaEntryBackend> = DataDirectory {
                path: test_path.clone(),
                data_dirs: None,
                quota_info: None,
            };

            // Removing non-existent directory should fail
            assert!(data_dir.remove().is_err());
        }

        #[test]
        fn test_set_block_limit_without_quota_info() {
            let temp_dir = TempDir::new().unwrap();
            let test_path = temp_dir.path().join("test_package");

            let data_dir =
                DataDirectory::<DefaultQuotaEntryBackend>::new(test_path, None, None).unwrap();

            // Should succeed but log a warning (quota_info is None)
            assert!(data_dir.set_block_limit(true).is_ok());
            assert!(data_dir.set_block_limit(false).is_ok());
        }

        #[test]
        fn test_quota_info_structure() {
            // Test that we can create quota info with different block limits
            let quota_info_with_limit = create_dummy_quota_info(Some(2048));
            assert!(quota_info_with_limit.block_limit.is_some());
            assert_eq!(quota_info_with_limit.block_limit.unwrap(), 2048);

            let quota_info_no_limit = create_dummy_quota_info(None);
            assert!(quota_info_no_limit.block_limit.is_none());

            // Test dummy quota entry operations
            assert!(
                quota_info_with_limit
                    .entry
                    .set_block_limits(100, 200)
                    .is_ok()
            );
        }

        #[test]
        fn test_path_returns_correct_path() {
            let temp_dir = TempDir::new().unwrap();
            let test_path = temp_dir.path().join("test_package");

            let data_dir =
                DataDirectory::<DefaultQuotaEntryBackend>::new(test_path.clone(), None, None)
                    .unwrap();

            assert_eq!(data_dir.path(), test_path.as_path());
        }

        #[test]
        fn test_data_directory_with_empty_data_dirs_string() {
            let temp_dir = TempDir::new().unwrap();
            let test_path = temp_dir.path().join("test_package");
            let data_dirs_str = Some("   ".to_string()); // Empty string with spaces

            let data_dir =
                DataDirectory::<DefaultQuotaEntryBackend>::new(test_path, data_dirs_str, None)
                    .unwrap();

            // Should parse to None (empty)
            assert!(data_dir.data_dirs().is_none());
        }

        #[test]
        fn test_ensure_data_dirs_with_complex_paths() {
            let temp_dir = TempDir::new().unwrap();
            let test_path = temp_dir.path().join("test_package");
            let data_dirs_str = Some("deep/nested/path:another/deep/path/here".to_string());

            let data_dir = DataDirectory::<DefaultQuotaEntryBackend>::new(
                test_path.clone(),
                data_dirs_str,
                None,
            )
            .unwrap();

            assert!(data_dir.ensure_data_dirs().is_ok());

            // Check that deeply nested directories were created
            assert!(test_path.join("deep/nested/path").exists());
            assert!(test_path.join("another/deep/path/here").exists());
        }

        fn create_failing_quota_info(
            block_limit: Option<u64>,
        ) -> QuotaInfo<mocks::MockQuotaEntryBackend> {
            let entry = mocks::MockQuotaEntryBackend::new(true);
            QuotaInfo::new(entry, block_limit)
        }

        #[test]
        fn test_set_block_limit_quota_entry_failure() {
            let temp_dir = TempDir::new().unwrap();
            let test_path = temp_dir.path().join("test_package");
            let failing_quota_info = Some(create_failing_quota_info(Some(2048)));

            let data_dir = DataDirectory::new(test_path, None, failing_quota_info).unwrap();

            // The quota entry operations will fail due to our failing mock
            let result = data_dir.set_block_limit(true);
            assert!(result.is_err());

            let result = data_dir.set_block_limit(false);
            assert!(result.is_err());
        }

        #[test]
        fn test_ensure_data_dirs_failure_cases() {
            let temp_dir = TempDir::new().unwrap();
            let test_path = temp_dir.path().join("test_package");

            // Create a file with the same name as one of the directories we want to create
            let conflicting_file = test_path.join("conflicting_dir");
            std::fs::create_dir_all(&test_path).unwrap();
            std::fs::write(&conflicting_file, "this is a file").unwrap();

            let data_dirs_str = Some("conflicting_dir/subdir".to_string());
            let data_dir =
                DataDirectory::<DefaultQuotaEntryBackend>::new(test_path, data_dirs_str, None)
                    .unwrap();

            // This should fail because we can't create a directory where a file exists
            let result = data_dir.ensure_data_dirs();
            assert!(result.is_err());
        }

        #[test]
        fn test_remove_with_manual_nonexistent_directory() {
            let temp_dir = TempDir::new().unwrap();
            let test_path = temp_dir.path().join("nonexistent");

            // Create DataDirectory without actually creating the directory
            let data_dir: DataDirectory<mocks::MockQuotaEntryBackend> = DataDirectory {
                path: test_path.clone(),
                data_dirs: None,
                quota_info: None,
            };

            // Removing non-existent directory should fail
            assert!(!test_path.exists(), "Test path should not exist initially");
            let result = data_dir.remove();
            assert!(result.is_err());
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

    mod quota_project_id_manager_test {
        use super::*;
        use std::path::PathBuf;
        use tokio::fs;

        #[tokio::test]
        async fn test_quota_project_id_manager_new() {
            let temp_dir = TempDir::new().unwrap();
            let data_root = temp_dir.path();

            // Create some test directories
            fs::create_dir(data_root.join("dir1")).await.unwrap();
            fs::create_dir(data_root.join("dir2")).await.unwrap();
            fs::create_dir(data_root.join("dir3")).await.unwrap();

            let manager =
                QuotaProjectIdManager::new(data_root, None::<&mocks::MockExt4QuotaManager>).await;

            assert_eq!(manager.project_map.len(), 3);
            assert!(manager.project_map.contains_key(&data_root.join("dir1")));
            assert!(manager.project_map.contains_key(&data_root.join("dir2")));
            assert!(manager.project_map.contains_key(&data_root.join("dir3")));
        }

        #[tokio::test]
        async fn test_quota_project_id_manager_new_empty_directory() {
            let temp_dir = TempDir::new().unwrap();
            let data_root = temp_dir.path();

            let manager =
                QuotaProjectIdManager::new(data_root, None::<&mocks::MockExt4QuotaManager>).await;

            assert_eq!(manager.project_map.len(), 0);
        }

        #[tokio::test]
        async fn test_quota_project_id_manager_new_with_files() {
            let temp_dir = TempDir::new().unwrap();
            let data_root = temp_dir.path();

            // Create both directories and files
            fs::create_dir(data_root.join("dir1")).await.unwrap();
            fs::create_dir(data_root.join("dir2")).await.unwrap();
            fs::write(data_root.join("file1.txt"), "content")
                .await
                .unwrap();
            fs::write(data_root.join("file2.txt"), "content")
                .await
                .unwrap();

            let manager =
                QuotaProjectIdManager::new(data_root, None::<&mocks::MockExt4QuotaManager>).await;

            // Only directories should be included, not files
            assert_eq!(manager.project_map.len(), 2);
            assert!(manager.project_map.contains_key(&data_root.join("dir1")));
            assert!(manager.project_map.contains_key(&data_root.join("dir2")));
            assert!(
                !manager
                    .project_map
                    .contains_key(&data_root.join("file1.txt"))
            );
            assert!(
                !manager
                    .project_map
                    .contains_key(&data_root.join("file2.txt"))
            );
        }

        #[tokio::test]
        async fn test_quota_project_id_manager_new_nonexistent_directory() {
            let temp_dir = TempDir::new().unwrap();
            let nonexistent_path = temp_dir.path().join("nonexistent");

            let manager =
                QuotaProjectIdManager::new(nonexistent_path, None::<&mocks::MockExt4QuotaManager>)
                    .await;

            // Should handle gracefully and create empty manager
            assert_eq!(manager.project_map.len(), 0);
        }

        #[tokio::test]
        async fn test_next_id() {
            let temp_dir = TempDir::new().unwrap();
            let data_root = temp_dir.path();

            // Test with empty directory (empty map)
            let manager =
                QuotaProjectIdManager::new(data_root, None::<&mocks::MockExt4QuotaManager>).await;
            assert_eq!(manager.next_id(), 1);

            // Test with some directories
            fs::create_dir(data_root.join("dir1")).await.unwrap();
            fs::create_dir(data_root.join("dir2")).await.unwrap();
            let manager =
                QuotaProjectIdManager::new(data_root, None::<&mocks::MockExt4QuotaManager>).await;

            // Should return max existing id + 1
            let max_id = manager.project_map.values().max().copied().unwrap_or(0);
            assert_eq!(manager.next_id(), max_id + 1);
        }

        #[tokio::test]
        async fn test_get_project_id_existing_path() {
            let temp_dir = TempDir::new().unwrap();
            let data_root = temp_dir.path();

            // Create a directory
            fs::create_dir(data_root.join("existing_dir"))
                .await
                .unwrap();
            let mut manager =
                QuotaProjectIdManager::new(data_root, None::<&mocks::MockExt4QuotaManager>).await;

            let existing_path = data_root.join("existing_dir");
            let original_id = manager.project_map.get(&existing_path).copied().unwrap();

            // Getting existing path should return the same ID
            let id = manager.get_project_id(&existing_path);
            assert_eq!(id, original_id);

            // Map size should remain the same
            assert_eq!(manager.project_map.len(), 1);
        }

        #[tokio::test]
        async fn test_get_project_id_new_path() {
            let temp_dir = TempDir::new().unwrap();
            let data_root = temp_dir.path();

            // Create initial directory
            fs::create_dir(data_root.join("existing_dir"))
                .await
                .unwrap();
            let mut manager =
                QuotaProjectIdManager::new(data_root, None::<&mocks::MockExt4QuotaManager>).await;

            let new_path = data_root.join("new_path");
            let expected_id = manager.next_id();

            // Getting new path should assign new ID
            let id = manager.get_project_id(&new_path);
            assert_eq!(id, expected_id);

            // Map should now contain the new entry
            assert_eq!(manager.project_map.len(), 2);
            assert_eq!(manager.project_map.get(&new_path), Some(&id));
        }

        #[tokio::test]
        async fn test_get_project_id_multiple_new_paths() {
            let temp_dir = TempDir::new().unwrap();
            let data_root = temp_dir.path();

            let mut manager =
                QuotaProjectIdManager::new(data_root, None::<&mocks::MockExt4QuotaManager>).await;

            let path1 = data_root.join("path1");
            let path2 = data_root.join("path2");
            let path3 = data_root.join("path3");

            let id1 = manager.get_project_id(&path1);
            let id2 = manager.get_project_id(&path2);
            let id3 = manager.get_project_id(&path3);

            assert_eq!(id1, 1);
            assert_eq!(id2, 2);
            assert_eq!(id3, 3);
            assert_eq!(manager.project_map.len(), 3);
        }

        #[tokio::test]
        async fn test_remove_id_existing_path() {
            let temp_dir = TempDir::new().unwrap();
            let data_root = temp_dir.path();

            // Create directories
            fs::create_dir(data_root.join("dir1")).await.unwrap();
            fs::create_dir(data_root.join("dir2")).await.unwrap();
            let mut manager =
                QuotaProjectIdManager::new(data_root, None::<&mocks::MockExt4QuotaManager>).await;

            let remove_path = data_root.join("dir1");
            let keep_path = data_root.join("dir2");

            // Remove one directory
            manager.remove_id(&remove_path);

            assert_eq!(manager.project_map.len(), 1);
            assert!(!manager.project_map.contains_key(&remove_path));
            assert!(manager.project_map.contains_key(&keep_path));
        }

        #[tokio::test]
        async fn test_remove_id_nonexistent_path() {
            let temp_dir = TempDir::new().unwrap();
            let data_root = temp_dir.path();

            // Create one directory
            fs::create_dir(data_root.join("existing_dir"))
                .await
                .unwrap();
            let mut manager =
                QuotaProjectIdManager::new(data_root, None::<&mocks::MockExt4QuotaManager>).await;

            let nonexistent_path = data_root.join("nonexistent");
            let original_len = manager.project_map.len();

            // Remove nonexistent path should not change anything
            manager.remove_id(&nonexistent_path);

            assert_eq!(manager.project_map.len(), original_len);
            assert!(
                manager
                    .project_map
                    .contains_key(&data_root.join("existing_dir"))
            );
        }

        #[tokio::test]
        async fn test_remove_id_empty_manager() {
            let temp_dir = TempDir::new().unwrap();
            let data_root = temp_dir.path();

            let mut manager =
                QuotaProjectIdManager::new(data_root, None::<&mocks::MockExt4QuotaManager>).await;

            // Should handle gracefully
            manager.remove_id(data_root.join("any_path"));
            assert_eq!(manager.project_map.len(), 0);
        }

        #[tokio::test]
        async fn test_complex_id_management_scenario() {
            let temp_dir = TempDir::new().unwrap();
            let data_root = temp_dir.path();

            let mut manager =
                QuotaProjectIdManager::new(data_root, None::<&mocks::MockExt4QuotaManager>).await;

            // Add several paths
            let path1 = data_root.join("path1");
            let path2 = data_root.join("path2");
            let path3 = data_root.join("path3");
            let path4 = data_root.join("path4");

            let id1 = manager.get_project_id(&path1);
            let id2 = manager.get_project_id(&path2);
            let id3 = manager.get_project_id(&path3);

            assert_eq!(id1, 1);
            assert_eq!(id2, 2);
            assert_eq!(id3, 3);

            // Remove the middle one
            manager.remove_id(&path2);

            // Next ID should be based on current max + 1
            let id4 = manager.get_project_id(&path4);
            assert_eq!(id4, 4); // max(1,3) + 1 = 4

            // Re-add path2, should get a new ID based on current max
            let id2_new = manager.get_project_id(&path2);
            assert_eq!(id2_new, 5); // max(1,3,4) + 1 = 5

            // Verify existing paths still have their original IDs
            assert_eq!(manager.get_project_id(&path1), 1);
            assert_eq!(manager.get_project_id(&path3), 3);
            assert_eq!(manager.get_project_id(&path4), 4);
        }

        #[tokio::test]
        async fn test_path_handling_variations() {
            let temp_dir = TempDir::new().unwrap();
            let data_root = temp_dir.path();

            let mut manager =
                QuotaProjectIdManager::new(data_root, None::<&mocks::MockExt4QuotaManager>).await;

            // Test string path
            let id1 = manager.get_project_id("/test/string/path");
            assert_eq!(id1, 1);
            assert!(
                manager
                    .project_map
                    .contains_key(&PathBuf::from("/test/string/path"))
            );

            // Test PathBuf
            let pathbuf = PathBuf::from("/test/pathbuf");
            let id2 = manager.get_project_id(&pathbuf);
            assert_eq!(id2, 2);
            assert!(manager.project_map.contains_key(&pathbuf));

            // Test case sensitivity
            let id3 = manager.get_project_id("/Test/Case");
            let id4 = manager.get_project_id("/test/case");
            assert_ne!(id3, id4); // Should be different (case sensitive)

            assert_eq!(manager.project_map.len(), 4);
        }

        #[tokio::test]
        async fn test_edge_cases() {
            let temp_dir = TempDir::new().unwrap();
            let data_root = temp_dir.path();

            let mut manager =
                QuotaProjectIdManager::new(data_root, None::<&mocks::MockExt4QuotaManager>).await;

            // Test removing and re-adding paths
            let path = data_root.join("test_path");
            let id1 = manager.get_project_id(&path);
            assert_eq!(id1, 1);

            manager.remove_id(&path);
            assert_eq!(manager.project_map.len(), 0);

            // Adding the same path again should start from 1
            let id2 = manager.get_project_id(&path);
            assert_eq!(id2, 1);

            // Add another path
            let path2 = data_root.join("path2");
            let id3 = manager.get_project_id(&path2);
            assert_eq!(id3, 2);
        }
    }

    mod data_directory_manager_tests {
        use super::*;

        #[tokio::test]
        async fn test_data_directory_manager_new_success() {
            let quota_impl = Some(super::mocks::MockExt4QuotaManager { should_fail: false });
            let data_root = TempDir::new().unwrap();
            let result = DataDirectoryManager::new(quota_impl, data_root).await;

            assert!(result.is_ok(), "DataDirectoryManager::new() should succeed");

            let manager = result.unwrap();

            // Verify quota_root is properly initialized
            assert!(
                manager.quota_root.is_some(),
                "quota_root should be initialized"
            );

            // Verify MockExt4QuotaManager is set to succeed in tests
            let quota = manager.quota_root.as_ref().unwrap();
            assert!(
                !quota.should_fail,
                "MockExt4QuotaManager should not be set to fail"
            );
        }

        #[tokio::test]
        async fn test_data_directory_manager_new_quota_enforce_failure() {
            // Test quota enforce failure using should_fail = true
            let quota_impl = Some(super::mocks::MockExt4QuotaManager { should_fail: true });
            let data_root = TempDir::new().unwrap();
            let result = DataDirectoryManager::new(quota_impl, data_root).await;

            // Should fail because quota.enforce() will return an error
            assert!(
                result.is_err(),
                "DataDirectoryManager::new() should fail when quota enforce fails"
            );

            let error = result.unwrap_err();
            assert!(error.to_string().contains("Failed to enforce quota"));
        }

        #[tokio::test]
        async fn test_data_directory_manager_components_initialization() {
            let quota_impl = Some(super::mocks::MockExt4QuotaManager { should_fail: false });
            let data_root = TempDir::new().unwrap();
            let result = DataDirectoryManager::new(quota_impl, data_root).await;
            assert!(result.is_ok());

            let manager = result.unwrap();

            assert!(manager.quota_root.is_some());
        }

        #[tokio::test]
        async fn test_create_data_directory_success() {
            let quota_impl = Some(super::mocks::MockExt4QuotaManager { should_fail: false });
            let temp_dir = TempDir::new().unwrap();
            let data_root = temp_dir.path();
            let mut manager = DataDirectoryManager::new(quota_impl, data_root)
                .await
                .unwrap();

            let test_path = data_root.join("test_package");
            let dir_meta = DataDirMetadata {
                path: test_path.clone(),
                package_name: "test-package".to_string(),
                data_dirs: Some("/app/data:/var/log".to_string()),
                storage_limit: Some(1024),
            };

            let result = manager.create_data_directory(dir_meta);

            assert!(result.is_ok(), "create_data_directory should succeed");

            let data_dir = result.unwrap();
            assert_eq!(data_dir.path(), test_path);
            assert!(test_path.exists());

            // Verify data directories were created
            assert!(test_path.join("app/data").exists());
            assert!(test_path.join("var/log").exists());
        }

        #[tokio::test]
        async fn test_create_data_directory_without_quota() {
            let quota_impl: Option<super::mocks::MockExt4QuotaManager> = None;
            let temp_dir = TempDir::new().unwrap();
            let data_root = temp_dir.path();
            let mut manager = DataDirectoryManager::new(quota_impl, data_root)
                .await
                .unwrap();

            let test_path = data_root.join("test_package_no_quota");
            let dir_meta = DataDirMetadata {
                path: test_path.clone(),
                package_name: "test-package-no-quota".to_string(),
                data_dirs: None,
                storage_limit: None,
            };

            let result = manager.create_data_directory(dir_meta);

            assert!(
                result.is_ok(),
                "create_data_directory should succeed without quota"
            );

            let data_dir = result.unwrap();
            assert_eq!(data_dir.path(), test_path);
            assert!(test_path.exists());
        }

        #[tokio::test]
        async fn test_create_data_directory_with_storage_limit() {
            let quota_impl = Some(super::mocks::MockExt4QuotaManager { should_fail: false });
            let temp_dir = TempDir::new().unwrap();
            let data_root = temp_dir.path();
            let mut manager = DataDirectoryManager::new(quota_impl, data_root)
                .await
                .unwrap();

            let test_path = data_root.join("test_package_with_limit");
            let dir_meta = DataDirMetadata {
                path: test_path.clone(),
                package_name: "test-package-limit".to_string(),
                data_dirs: None,
                storage_limit: Some(2048),
            };

            let result = manager.create_data_directory(dir_meta);

            assert!(
                result.is_ok(),
                "create_data_directory should succeed with storage limit"
            );

            let data_dir = result.unwrap();
            assert_eq!(data_dir.path(), test_path);

            // Verify quota info is set
            assert!(data_dir.quota_info.is_some());
            assert_eq!(
                data_dir.quota_info.as_ref().unwrap().block_limit,
                Some(2048)
            );
        }

        #[tokio::test]
        async fn test_remove_data_directory_success() {
            let quota_impl = Some(super::mocks::MockExt4QuotaManager { should_fail: false });
            let temp_dir = TempDir::new().unwrap();
            let data_root = temp_dir.path();
            let mut manager = DataDirectoryManager::new(quota_impl, data_root)
                .await
                .unwrap();

            // First create a data directory
            let test_path = data_root.join("test_package_remove");
            let dir_meta = DataDirMetadata {
                path: test_path.clone(),
                package_name: "test-package-remove".to_string(),
                data_dirs: Some("/app/data".to_string()),
                storage_limit: Some(1024),
            };

            let data_dir = manager.create_data_directory(dir_meta).unwrap();
            assert!(test_path.exists());

            // Now remove it
            let result = manager.remove_data_directory(&data_dir);

            assert!(result.is_ok(), "remove_data_directory should succeed");
            assert!(!test_path.exists(), "Directory should be removed");
        }

        #[tokio::test]
        async fn test_remove_data_directory_quota_failure() {
            let quota_impl = Some(super::mocks::MockExt4QuotaManager { should_fail: false });
            let temp_dir = TempDir::new().unwrap();
            let data_root = temp_dir.path();
            let mut manager = DataDirectoryManager::new(quota_impl, data_root)
                .await
                .unwrap();

            // Create a data directory with failing quota entry
            let test_path = data_root.join("test_package_quota_fail");
            let failing_quota_info = Some(super::QuotaInfo::new(
                super::mocks::MockQuotaEntryBackend::new(true),
                Some(1024),
            ));

            let data_dir = DataDirectory::<super::mocks::MockQuotaEntryBackend> {
                path: test_path.clone(),
                data_dirs: None,
                quota_info: failing_quota_info,
            };

            // Create the directory manually since we can't use create_data_directory with failing quota
            std::fs::create_dir_all(&test_path).unwrap();

            // Remove should still succeed even if quota operations fail
            let result = manager.remove_data_directory(&data_dir);

            assert!(
                result.is_ok(),
                "remove_data_directory should succeed even with quota failure"
            );
            assert!(!test_path.exists(), "Directory should be removed");
        }

        #[tokio::test]
        async fn test_remove_data_directory_nonexistent() {
            let quota_impl = Some(super::mocks::MockExt4QuotaManager { should_fail: false });
            let temp_dir = TempDir::new().unwrap();
            let data_root = temp_dir.path();
            let mut manager = DataDirectoryManager::new(quota_impl, data_root)
                .await
                .unwrap();

            // Create DataDirectory without actually creating the directory
            let nonexistent_path = data_root.join("nonexistent");
            let data_dir: DataDirectory<super::mocks::MockQuotaEntryBackend> = DataDirectory {
                path: nonexistent_path.clone(),
                data_dirs: None,
                quota_info: None,
            };

            let result = manager.remove_data_directory(&data_dir);

            assert!(
                result.is_err(),
                "remove_data_directory should fail for nonexistent directory"
            );
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
