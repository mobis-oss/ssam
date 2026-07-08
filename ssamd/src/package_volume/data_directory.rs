// Copyright 2025-2026 Hyundai Mobis Co., Ltd.
// SPDX-License-Identifier: Apache-2.0

use crate::configuration;
use crate::ext4quota::Ext4QuotaEntry;
use crate::utils::{self, quota_utils};
use anyhow::Context;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::PackageFileInfo;

pub(super) fn parse_data_dirs(data_dirs_str: Option<String>) -> Option<Vec<PathBuf>> {
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

pub trait QuotaEntryBackend {
    fn set_block_limits(&self, soft_limit: u64, hard_limit: u64) -> anyhow::Result<()>;
    fn set_project_quota(&self, path: &Path, enabled: bool) -> anyhow::Result<()>;
}

#[derive(Debug)]
pub struct DefaultQuotaEntryBackend {
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
    pub(super) entry: T,
    pub(super) block_limit: Option<u64>,
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
pub struct DataDirectory<T: QuotaEntryBackend> {
    pub(super) path: PathBuf,
    pub(super) data_dirs: Option<Vec<PathBuf>>,
    pub(super) quota_info: Option<QuotaInfo<T>>,
}

impl<T: QuotaEntryBackend> DataDirectory<T> {
    pub(crate) fn new(
        path: PathBuf,
        data_dirs_str: Option<String>,
        quota_info: Option<QuotaInfo<T>>,
    ) -> anyhow::Result<Self> {
        utils::make_directory(&path, true)
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
                utils::make_directory(&src_path, true).with_context(|| {
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
    pub(super) package_name: String,
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
pub(super) struct QuotaProjectIdManager {
    pub(super) project_map: HashMap<PathBuf, usize>,
}

impl QuotaProjectIdManager {
    pub(super) async fn new<Q: Ext4QuotaBackend>(
        data_root: impl AsRef<Path>,
        quota: Option<&Q>,
    ) -> Self {
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

    pub(super) fn next_id(&self) -> usize {
        self.project_map.values().max().copied().unwrap_or_default() + 1
    }

    pub(super) fn get_project_id(&mut self, path: impl AsRef<Path>) -> usize {
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

    pub(super) fn remove_id(&mut self, path: impl AsRef<Path>) {
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
    pub(super) quota_root: Option<Q>,
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
pub(crate) struct Ext4Quota {
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
        quota_utils::get_active_projid(path).ok().flatten()
    }
}

impl<Q: Ext4QuotaBackend> DataDirectoryManager<Q> {
    pub(crate) async fn new(
        quota_root: Option<Q>,
        data_root: impl AsRef<Path>,
    ) -> anyhow::Result<Self> {
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

    pub(crate) fn create_data_directory(
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

    pub(crate) fn remove_data_directory(
        &mut self,
        data_dir: &DataDirectory<Q::Entry>,
    ) -> anyhow::Result<()> {
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
            type Entry = MockQuotaEntryBackend;

            fn enforce(&self) -> anyhow::Result<()> {
                if self.should_fail {
                    anyhow::bail!("Dummy enforce failed")
                }
                Ok(())
            }

            fn entry(&self, _id: i32) -> Self::Entry {
                MockQuotaEntryBackend::new(self.should_fail)
            }

            fn get_project_id_for_dir(&self, _path: &Path) -> Option<usize> {
                Some(1)
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

            assert!(test_path.exists());
            assert!(data_dir.remove().is_ok());
            assert!(!test_path.exists());
        }

        #[test]
        fn test_remove_nonexistent_directory_fails() {
            let temp_dir = TempDir::new().unwrap();
            let test_path = temp_dir.path().join("nonexistent");

            let data_dir: DataDirectory<mocks::MockQuotaEntryBackend> = DataDirectory {
                path: test_path.clone(),
                data_dirs: None,
                quota_info: None,
            };

            assert!(data_dir.remove().is_err());
        }

        #[test]
        fn test_set_block_limit_without_quota_info() {
            let temp_dir = TempDir::new().unwrap();
            let test_path = temp_dir.path().join("test_package");

            let data_dir =
                DataDirectory::<DefaultQuotaEntryBackend>::new(test_path, None, None).unwrap();

            assert!(data_dir.set_block_limit(true).is_ok());
            assert!(data_dir.set_block_limit(false).is_ok());
        }

        #[test]
        fn test_quota_info_structure() {
            let quota_info_with_limit = create_dummy_quota_info(Some(2048));
            assert!(quota_info_with_limit.block_limit.is_some());
            assert_eq!(quota_info_with_limit.block_limit.unwrap(), 2048);

            let quota_info_no_limit = create_dummy_quota_info(None);
            assert!(quota_info_no_limit.block_limit.is_none());

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
            let data_dirs_str = Some("   ".to_string());

            let data_dir =
                DataDirectory::<DefaultQuotaEntryBackend>::new(test_path, data_dirs_str, None)
                    .unwrap();

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

            let result = data_dir.set_block_limit(true);
            assert!(result.is_err());

            let result = data_dir.set_block_limit(false);
            assert!(result.is_err());
        }

        #[test]
        fn test_ensure_data_dirs_failure_cases() {
            let temp_dir = TempDir::new().unwrap();
            let test_path = temp_dir.path().join("test_package");

            let conflicting_file = test_path.join("conflicting_dir");
            std::fs::create_dir_all(&test_path).unwrap();
            std::fs::write(&conflicting_file, "this is a file").unwrap();

            let data_dirs_str = Some("conflicting_dir/subdir".to_string());
            let data_dir =
                DataDirectory::<DefaultQuotaEntryBackend>::new(test_path, data_dirs_str, None)
                    .unwrap();

            let result = data_dir.ensure_data_dirs();
            assert!(result.is_err());
        }

        #[test]
        fn test_remove_with_manual_nonexistent_directory() {
            let temp_dir = TempDir::new().unwrap();
            let test_path = temp_dir.path().join("nonexistent");

            let data_dir: DataDirectory<mocks::MockQuotaEntryBackend> = DataDirectory {
                path: test_path.clone(),
                data_dirs: None,
                quota_info: None,
            };

            assert!(!test_path.exists(), "Test path should not exist initially");
            let result = data_dir.remove();
            assert!(result.is_err());
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

            assert_eq!(manager.project_map.len(), 0);
        }

        #[tokio::test]
        async fn test_next_id() {
            let temp_dir = TempDir::new().unwrap();
            let data_root = temp_dir.path();

            let manager =
                QuotaProjectIdManager::new(data_root, None::<&mocks::MockExt4QuotaManager>).await;
            assert_eq!(manager.next_id(), 1);

            fs::create_dir(data_root.join("dir1")).await.unwrap();
            fs::create_dir(data_root.join("dir2")).await.unwrap();
            let manager =
                QuotaProjectIdManager::new(data_root, None::<&mocks::MockExt4QuotaManager>).await;

            let max_id = manager.project_map.values().max().copied().unwrap_or(0);
            assert_eq!(manager.next_id(), max_id + 1);
        }

        #[tokio::test]
        async fn test_get_project_id_existing_path() {
            let temp_dir = TempDir::new().unwrap();
            let data_root = temp_dir.path();

            fs::create_dir(data_root.join("existing_dir"))
                .await
                .unwrap();
            let mut manager =
                QuotaProjectIdManager::new(data_root, None::<&mocks::MockExt4QuotaManager>).await;

            let existing_path = data_root.join("existing_dir");
            let original_id = manager.project_map.get(&existing_path).copied().unwrap();

            let id = manager.get_project_id(&existing_path);
            assert_eq!(id, original_id);

            assert_eq!(manager.project_map.len(), 1);
        }

        #[tokio::test]
        async fn test_get_project_id_new_path() {
            let temp_dir = TempDir::new().unwrap();
            let data_root = temp_dir.path();

            fs::create_dir(data_root.join("existing_dir"))
                .await
                .unwrap();
            let mut manager =
                QuotaProjectIdManager::new(data_root, None::<&mocks::MockExt4QuotaManager>).await;

            let new_path = data_root.join("new_path");
            let expected_id = manager.next_id();

            let id = manager.get_project_id(&new_path);
            assert_eq!(id, expected_id);

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

            fs::create_dir(data_root.join("dir1")).await.unwrap();
            fs::create_dir(data_root.join("dir2")).await.unwrap();
            let mut manager =
                QuotaProjectIdManager::new(data_root, None::<&mocks::MockExt4QuotaManager>).await;

            let remove_path = data_root.join("dir1");
            let keep_path = data_root.join("dir2");

            manager.remove_id(&remove_path);

            assert_eq!(manager.project_map.len(), 1);
            assert!(!manager.project_map.contains_key(&remove_path));
            assert!(manager.project_map.contains_key(&keep_path));
        }

        #[tokio::test]
        async fn test_remove_id_nonexistent_path() {
            let temp_dir = TempDir::new().unwrap();
            let data_root = temp_dir.path();

            fs::create_dir(data_root.join("existing_dir"))
                .await
                .unwrap();
            let mut manager =
                QuotaProjectIdManager::new(data_root, None::<&mocks::MockExt4QuotaManager>).await;

            let nonexistent_path = data_root.join("nonexistent");
            let original_len = manager.project_map.len();

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

            manager.remove_id(data_root.join("any_path"));
            assert_eq!(manager.project_map.len(), 0);
        }

        #[tokio::test]
        async fn test_complex_id_management_scenario() {
            let temp_dir = TempDir::new().unwrap();
            let data_root = temp_dir.path();

            let mut manager =
                QuotaProjectIdManager::new(data_root, None::<&mocks::MockExt4QuotaManager>).await;

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

            manager.remove_id(&path2);

            let id4 = manager.get_project_id(&path4);
            assert_eq!(id4, 4);

            let id2_new = manager.get_project_id(&path2);
            assert_eq!(id2_new, 5);

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

            let id1 = manager.get_project_id("/test/string/path");
            assert_eq!(id1, 1);
            assert!(
                manager
                    .project_map
                    .contains_key(&PathBuf::from("/test/string/path"))
            );

            let pathbuf = PathBuf::from("/test/pathbuf");
            let id2 = manager.get_project_id(&pathbuf);
            assert_eq!(id2, 2);
            assert!(manager.project_map.contains_key(&pathbuf));

            let id3 = manager.get_project_id("/Test/Case");
            let id4 = manager.get_project_id("/test/case");
            assert_ne!(id3, id4);

            assert_eq!(manager.project_map.len(), 4);
        }

        #[tokio::test]
        async fn test_edge_cases() {
            let temp_dir = TempDir::new().unwrap();
            let data_root = temp_dir.path();

            let mut manager =
                QuotaProjectIdManager::new(data_root, None::<&mocks::MockExt4QuotaManager>).await;

            let path = data_root.join("test_path");
            let id1 = manager.get_project_id(&path);
            assert_eq!(id1, 1);

            manager.remove_id(&path);
            assert_eq!(manager.project_map.len(), 0);

            let id2 = manager.get_project_id(&path);
            assert_eq!(id2, 1);

            let path2 = data_root.join("path2");
            let id3 = manager.get_project_id(&path2);
            assert_eq!(id3, 2);
        }
    }

    mod data_directory_manager_tests {
        use super::*;

        #[tokio::test]
        async fn test_data_directory_manager_new_success() {
            let quota_impl = Some(mocks::MockExt4QuotaManager { should_fail: false });
            let data_root = TempDir::new().unwrap();
            let result = DataDirectoryManager::new(quota_impl, data_root).await;

            assert!(result.is_ok(), "DataDirectoryManager::new() should succeed");

            let manager = result.unwrap();

            assert!(
                manager.quota_root.is_some(),
                "quota_root should be initialized"
            );

            let quota = manager.quota_root.as_ref().unwrap();
            assert!(
                !quota.should_fail,
                "MockExt4QuotaManager should not be set to fail"
            );
        }

        #[tokio::test]
        async fn test_data_directory_manager_new_quota_enforce_failure() {
            let quota_impl = Some(mocks::MockExt4QuotaManager { should_fail: true });
            let data_root = TempDir::new().unwrap();
            let result = DataDirectoryManager::new(quota_impl, data_root).await;

            assert!(
                result.is_err(),
                "DataDirectoryManager::new() should fail when quota enforce fails"
            );

            let error = result.unwrap_err();
            assert!(error.to_string().contains("Failed to enforce quota"));
        }

        #[tokio::test]
        async fn test_data_directory_manager_components_initialization() {
            let quota_impl = Some(mocks::MockExt4QuotaManager { should_fail: false });
            let data_root = TempDir::new().unwrap();
            let result = DataDirectoryManager::new(quota_impl, data_root).await;
            assert!(result.is_ok());

            let manager = result.unwrap();

            assert!(manager.quota_root.is_some());
        }

        #[tokio::test]
        async fn test_create_data_directory_success() {
            let quota_impl = Some(mocks::MockExt4QuotaManager { should_fail: false });
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

            assert!(test_path.join("app/data").exists());
            assert!(test_path.join("var/log").exists());
        }

        #[tokio::test]
        async fn test_create_data_directory_without_quota() {
            let quota_impl: Option<mocks::MockExt4QuotaManager> = None;
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
            let quota_impl = Some(mocks::MockExt4QuotaManager { should_fail: false });
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

            assert!(data_dir.quota_info.is_some());
            assert_eq!(
                data_dir.quota_info.as_ref().unwrap().block_limit,
                Some(2048)
            );
        }

        #[tokio::test]
        async fn test_remove_data_directory_success() {
            let quota_impl = Some(mocks::MockExt4QuotaManager { should_fail: false });
            let temp_dir = TempDir::new().unwrap();
            let data_root = temp_dir.path();
            let mut manager = DataDirectoryManager::new(quota_impl, data_root)
                .await
                .unwrap();

            let test_path = data_root.join("test_package_remove");
            let dir_meta = DataDirMetadata {
                path: test_path.clone(),
                package_name: "test-package-remove".to_string(),
                data_dirs: Some("/app/data".to_string()),
                storage_limit: Some(1024),
            };

            let data_dir = manager.create_data_directory(dir_meta).unwrap();
            assert!(test_path.exists());

            let result = manager.remove_data_directory(&data_dir);

            assert!(result.is_ok(), "remove_data_directory should succeed");
            assert!(!test_path.exists(), "Directory should be removed");
        }

        #[tokio::test]
        async fn test_remove_data_directory_quota_failure() {
            let quota_impl = Some(mocks::MockExt4QuotaManager { should_fail: false });
            let temp_dir = TempDir::new().unwrap();
            let data_root = temp_dir.path();
            let mut manager = DataDirectoryManager::new(quota_impl, data_root)
                .await
                .unwrap();

            let test_path = data_root.join("test_package_quota_fail");
            let failing_quota_info = Some(QuotaInfo::new(
                mocks::MockQuotaEntryBackend::new(true),
                Some(1024),
            ));

            let data_dir = DataDirectory::<mocks::MockQuotaEntryBackend> {
                path: test_path.clone(),
                data_dirs: None,
                quota_info: failing_quota_info,
            };

            std::fs::create_dir_all(&test_path).unwrap();

            let result = manager.remove_data_directory(&data_dir);

            assert!(
                result.is_ok(),
                "remove_data_directory should succeed even with quota failure"
            );
            assert!(!test_path.exists(), "Directory should be removed");
        }

        #[tokio::test]
        async fn test_remove_data_directory_nonexistent() {
            let quota_impl = Some(mocks::MockExt4QuotaManager { should_fail: false });
            let temp_dir = TempDir::new().unwrap();
            let data_root = temp_dir.path();
            let mut manager = DataDirectoryManager::new(quota_impl, data_root)
                .await
                .unwrap();

            let nonexistent_path = data_root.join("nonexistent");
            let data_dir: DataDirectory<mocks::MockQuotaEntryBackend> = DataDirectory {
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
}
