// Copyright 2025-2026 Hyundai Mobis Co., Ltd.
// SPDX-License-Identifier: Apache-2.0

//! ext4quota: Manage ext4 disk quotas on Linux
//!
//! This module provides a safe and ergonomic Rust API for managing ext4 filesystem quotas.
//! It supports user, group, and project quotas, and allows you to:
//!
//! - Query and set block/inode soft and hard limits per id
//! - Query and set grace times (global, per quota type)
//! - Query current usage (blocks/inodes) per id
//! - Enable, disable, and check quota enforcement/accounting (per quota type)
//!
//! # Example
//! ```rust,ignore
//! use crate::ext4quota::{Ext4Quota, QuotaType};
//!
//! // Create a quota handle for a mount point and quota type
//! let quota = Ext4Quota::new("/mnt/data", QuotaType::User).unwrap();
//! // Get an entry for a specific user/group/project id
//! let entry = quota.entry(1000);
//! // Set block limits (soft, hard) in KB
//! entry.set_block_limits(10240, 20480).unwrap();
//! // Query current usage
//! let (soft, hard) = entry.get_block_limit().unwrap();
//! println!("Block soft limit: {}, hard limit: {}", soft, hard);
//! // Set grace time (global, for this quota type)
//! quota.set_block_grace(std::time::Duration::from_secs(3600)).unwrap();
//! ```
//!
//! # Requirements
//! - Linux with ext4 filesystem and quota support enabled
//! - Root privileges for most operations
//!
//! # Notes
//! - Project quota support requires recent kernel and e2fsprogs
//! - Some features may require specific mount options

use procfs::MountEntry;
pub use quotactl_rs::QuotaType;
use quotactl_rs::quota;
use quotactl_rs::quota::InfoValid;
pub use quotactl_rs::quota::{DqBlk, DqInfo, DqInfoFlags, QuotaValid};
use quotactl_rs::xfs_quota::{FsQuotaStat, FsQuotaStateFlags};
use rustix::io::Errno;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("Failed to find mount entry for '{path}': {source:?}")]
    MountEntryNotFound {
        path: PathBuf,
        #[source]
        source: Option<io::Error>,
    },
    #[error("Not supported filesystem '{path}': {reason}")]
    NotSupported { path: PathBuf, reason: String },
    #[error("Quota operation failed for {quota_type:?} (id: {id}) on '{mount_point}': {source}")]
    QuotaOpFailed {
        mount_point: PathBuf,
        quota_type: QuotaType,
        id: i32,
        #[source]
        source: io::Error,
    },
    #[error("Quota is not enabled for {quota_type:?} on '{mount_point}'")]
    QuotaNotEnabled {
        mount_point: PathBuf,
        quota_type: QuotaType,
    },
    #[error("Io Error: {0}")]
    Io(#[from] io::Error),
    #[error("Procfs error: {source}")]
    ProcError {
        #[from]
        source: procfs::ProcError,
    },
}

// Define a convenient Result type
pub type Result<T> = std::result::Result<T, Error>;

// --- Mount Information Parsing (Using procfs) ---

fn get_mount_entry_from_path(mount_point: impl AsRef<Path>) -> Result<MountEntry> {
    let mount_point = mount_point.as_ref();

    let canonical_path = mount_point
        .canonicalize()
        .map_err(|e| Error::MountEntryNotFound {
            path: mount_point.to_path_buf(),
            source: Some(e),
        })?;

    let mounts = procfs::mounts()?;

    mounts
        .into_iter()
        .find_map(|entry| {
            let entry_mount_point = Path::new(&entry.fs_file).canonicalize().ok()?;
            if entry_mount_point == canonical_path {
                Some(entry)
            } else {
                None
            }
        })
        .ok_or_else(|| Error::MountEntryNotFound {
            path: mount_point.to_path_buf(),
            source: None,
        })
}

fn is_accounting(flags: FsQuotaStateFlags, quota_type: QuotaType) -> bool {
    match quota_type {
        QuotaType::User => flags.contains(FsQuotaStateFlags::FS_QUOTA_UDQ_ACCT),
        QuotaType::Group => flags.contains(FsQuotaStateFlags::FS_QUOTA_GDQ_ACCT),
        QuotaType::Project => flags.contains(FsQuotaStateFlags::FS_QUOTA_PDQ_ACCT),
    }
}

fn is_enforced(flags: FsQuotaStateFlags, quota_type: QuotaType) -> bool {
    match quota_type {
        QuotaType::User => flags.contains(FsQuotaStateFlags::FS_QUOTA_UDQ_ENFD),
        QuotaType::Group => flags.contains(FsQuotaStateFlags::FS_QUOTA_GDQ_ENFD),
        QuotaType::Project => flags.contains(FsQuotaStateFlags::FS_QUOTA_PDQ_ENFD),
    }
}

fn err_is_enosys(e: &io::Error) -> bool {
    e.raw_os_error()
        .is_some_and(|code| Errno::from_raw_os_error(code) == Errno::NOSYS)
}

// --- Public Object-Oriented API ---
/// Handle for ext4 quota management on a specific mount point and quota type (user/group/project).
///
/// Use this struct to check/enforce quota, set/get grace times, and create entry handles for specific ids.
///
/// # Fields
/// * `mount_point` - Path to the ext4 mount point.
/// * `device_path` - Path to the device associated with the mount point.
/// * `quota_type` - Type of quota (User, Group, Project).
#[derive(Debug)]
pub struct Ext4Quota {
    mount_point: PathBuf,
    device_path: PathBuf,
    quota_type: QuotaType,
}

impl Ext4Quota {
    /// Create a new `Ext4Quota` for the given mount point and quota type.
    ///
    /// # Arguments
    /// * `mount_point` - Path to the ext4 mount point
    /// * `quota_type` - `QuotaType` (User, Group, Project)
    ///
    /// # Errors
    /// Returns an error if the mount is not ext4, quota is not enabled, or system calls fail.
    pub fn new(mount_point: impl AsRef<Path>, quota_type: QuotaType) -> Result<Self> {
        let mount_point = mount_point.as_ref();
        let mount_entry = get_mount_entry_from_path(mount_point)?;
        let device_path = PathBuf::from(&mount_entry.fs_spec);

        if mount_entry.fs_vfstype != "ext4" {
            return Err(Error::NotSupported {
                path: mount_point.to_path_buf(),
                reason: format!("Unsupported filesystem type: {}", mount_entry.fs_vfstype),
            });
        }

        let stat = quotactl_rs::xfs_quota::x_get_qstat(quota_type, &device_path).map_err(|e| {
            if err_is_enosys(&e) {
                Error::NotSupported {
                    path: mount_point.to_path_buf(),
                    reason: "Legacy quota is not supported".to_string(),
                }
            } else {
                Error::QuotaOpFailed {
                    mount_point: mount_point.to_path_buf(),
                    quota_type,
                    id: 0,
                    source: e,
                }
            }
        })?;

        if !is_accounting(stat.flags, quota_type) {
            return Err(Error::QuotaNotEnabled {
                quota_type,
                mount_point: mount_point.to_path_buf(),
            });
        }

        Ok(Self {
            mount_point: mount_point.to_path_buf(),
            device_path,
            quota_type,
        })
    }

    /// Create an entry handle for a specific id (user/group/project id).
    ///
    /// # Arguments
    /// * `id` - The user/group/project id to operate on
    #[must_use]
    pub fn entry(&self, id: i32) -> Ext4QuotaEntry {
        Ext4QuotaEntry::new(
            self.mount_point.clone(),
            self.device_path.clone(),
            self.quota_type,
            id,
        )
    }

    fn get_status(&self) -> Result<FsQuotaStat> {
        let stat = quotactl_rs::xfs_quota::x_get_qstat(self.quota_type, self.device_path.as_path())
            .map_err(|e| {
                if err_is_enosys(&e) {
                    Error::NotSupported {
                        path: self.mount_point.clone(),
                        reason: "Legacy quota is not supported".to_string(),
                    }
                } else {
                    Error::QuotaOpFailed {
                        mount_point: self.mount_point.clone(),
                        quota_type: self.quota_type,
                        id: 0,
                        source: e,
                    }
                }
            })?;
        Ok(stat)
    }

    /// Check if quota enforcement is enabled for this quota type.
    ///
    /// # Returns
    /// `true` if enforced, `false` otherwise.
    ///
    /// # Errors
    /// Returns `Error::QuotaOpFailed` if the `quotactl` system call fails, or
    /// `Error::NotSupported` if the kernel does not support XFS-style quota ioctls.
    pub fn is_enforced(&self) -> Result<bool> {
        let stat = self.get_status()?;

        // Check if the quota is enforced
        Ok(is_enforced(stat.flags, self.quota_type))
    }

    /// Check if quota accounting is enabled for this quota type.
    ///
    /// # Returns
    /// `true` if accounting is enabled, `false` otherwise.
    ///
    /// # Errors
    /// Returns `Error::QuotaOpFailed` if the `quotactl` system call fails, or
    /// `Error::NotSupported` if the kernel does not support XFS-style quota ioctls.
    pub fn is_accounting(&self) -> Result<bool> {
        let stat = self.get_status()?;

        Ok(is_accounting(stat.flags, self.quota_type))
    }

    /// Enable quota enforcement for this quota type.
    ///
    /// # Errors
    /// Returns `Error::QuotaOpFailed` or `Error::NotSupported` if the status check fails.
    /// Returns `Error::NotSupported` if the kernel does not support XFS-style quota ioctls.
    /// Returns `Error::Io` if the `quotactl` `x_quota_on` system call fails.
    pub fn enforce(&self) -> Result<()> {
        if self.is_enforced()? {
            return Ok(());
        }

        quotactl_rs::xfs_quota::x_quota_on(self.quota_type, self.device_path.as_path()).map_err(
            |e| {
                if err_is_enosys(&e) {
                    Error::NotSupported {
                        path: self.mount_point.clone(),
                        reason: "Legacy quota is not supported".to_string(),
                    }
                } else {
                    e.into()
                }
            },
        )
    }

    /// Disable quota enforcement for this quota type.
    ///
    /// # Errors
    /// Returns `Error::QuotaOpFailed` or `Error::NotSupported` if the status check fails.
    /// Returns `Error::NotSupported` if the kernel does not support XFS-style quota ioctls.
    /// Returns `Error::Io` if the `quotactl` `x_quota_off` system call fails.
    pub fn unenforce(&self) -> Result<()> {
        if !self.is_enforced()? {
            return Ok(());
        }

        quotactl_rs::xfs_quota::x_quota_off(self.quota_type, self.device_path.as_path()).map_err(
            |e| {
                if err_is_enosys(&e) {
                    Error::NotSupported {
                        path: self.mount_point.clone(),
                        reason: "Legacy quota is not supported".to_string(),
                    }
                } else {
                    e.into()
                }
            },
        )
    }

    /// Set the block grace time (soft limit exceedance period) for this quota type (global).
    ///
    /// # Arguments
    /// * `duration` - Duration of the grace period
    ///
    /// # Errors
    /// Returns `Error::QuotaOpFailed` if the `quotactl` `set_info` system call fails.
    pub fn set_block_grace(&self, duration: Duration) -> Result<()> {
        let dqinfo = DqInfo {
            dqi_bgrace: duration.as_secs(),
            dqi_valid: InfoValid::BGRACE.bits(),
            ..Default::default()
        };
        quotactl_rs::quota::set_info(self.quota_type, &self.device_path, dqinfo).map_err(|e| {
            Error::QuotaOpFailed {
                mount_point: self.mount_point.clone(),
                quota_type: self.quota_type,
                id: 0, // global, not per-id
                source: e,
            }
        })
    }

    /// Set the inode grace time (soft limit exceedance period) for this quota type (global).
    ///
    /// # Arguments
    /// * `duration` - Duration of the grace period
    ///
    /// # Errors
    /// Returns `Error::QuotaOpFailed` if the `quotactl` `set_info` system call fails.
    pub fn set_inode_grace(&self, duration: Duration) -> Result<()> {
        let dqinfo = DqInfo {
            dqi_igrace: duration.as_secs(),
            dqi_valid: InfoValid::IGRACE.bits(),
            ..Default::default()
        };
        quotactl_rs::quota::set_info(self.quota_type, &self.device_path, dqinfo).map_err(|e| {
            Error::QuotaOpFailed {
                mount_point: self.mount_point.clone(),
                quota_type: self.quota_type,
                id: 0, // global, not per-id
                source: e,
            }
        })
    }

    /// Get the block grace time (soft limit exceedance period) for this quota type (global).
    ///
    /// # Returns
    /// Duration of the block grace period.
    ///
    /// # Errors
    /// Returns `Error::QuotaOpFailed` if the `quotactl` `get_info` system call fails.
    pub fn get_block_grace(&self) -> Result<Duration> {
        let dqinfo =
            quotactl_rs::quota::get_info(self.quota_type, &self.device_path).map_err(|e| {
                Error::QuotaOpFailed {
                    mount_point: self.mount_point.clone(),
                    quota_type: self.quota_type,
                    id: 0,
                    source: e,
                }
            })?;
        Ok(Duration::from_secs(dqinfo.dqi_bgrace))
    }

    /// Get the inode grace time (soft limit exceedance period) for this quota type (global).
    ///
    /// # Returns
    /// Duration of the inode grace period.
    ///
    /// # Errors
    /// Returns `Error::QuotaOpFailed` if the `quotactl` `get_info` system call fails.
    pub fn get_inode_grace(&self) -> Result<Duration> {
        let dqinfo =
            quotactl_rs::quota::get_info(self.quota_type, &self.device_path).map_err(|e| {
                Error::QuotaOpFailed {
                    mount_point: self.mount_point.clone(),
                    quota_type: self.quota_type,
                    id: 0,
                    source: e,
                }
            })?;
        Ok(Duration::from_secs(dqinfo.dqi_igrace))
    }

    /// Get the mount point path.
    #[must_use]
    pub fn mount_point(&self) -> &Path {
        &self.mount_point
    }

    /// Get the quota type.
    #[must_use]
    pub fn quota_type(&self) -> QuotaType {
        self.quota_type
    }
}

/// Handle for per-id (user/group/project) quota operations.
///
/// Use this struct to set/get block/inode limits, timers, and usage for a specific id.
#[derive(Debug)]
pub struct Ext4QuotaEntry {
    mount_point: PathBuf,
    device_path: PathBuf,
    quota_type: QuotaType,
    id: i32,
}

impl Ext4QuotaEntry {
    /// Create a new `Ext4QuotaEntry`. This is private and can only be used internally.
    ///
    /// # Arguments
    /// * `device_path` - The path to the device
    /// * `quota_type` - The type of quota (user/group/project)
    /// * `id` - The user/group/project id
    fn new(mount_point: PathBuf, device_path: PathBuf, quota_type: QuotaType, id: i32) -> Self {
        Ext4QuotaEntry {
            mount_point,
            device_path,
            quota_type,
            id,
        }
    }

    fn set_quota_internal(&self, dqblk: DqBlk) -> Result<()> {
        quota::set_quota(self.quota_type, &self.device_path, self.id, dqblk).map_err(|e| {
            Error::QuotaOpFailed {
                mount_point: self.mount_point.clone(),
                quota_type: self.quota_type,
                id: self.id,
                source: e,
            }
        })?;
        Ok(())
    }

    /// Set block (space) soft and hard limits (in kilobytes) for this id.
    ///
    /// # Arguments
    /// * `soft_limit_kb` - Soft limit in kilobytes
    /// * `hard_limit_kb` - Hard limit in kilobytes
    ///
    /// # Errors
    /// Returns `Error::QuotaOpFailed` if the `quotactl` `set_quota` system call fails.
    pub fn set_block_limits(&self, soft_limit_kb: u64, hard_limit_kb: u64) -> Result<()> {
        let dqblk = DqBlk {
            dqb_bhardlimit: hard_limit_kb,
            dqb_bsoftlimit: soft_limit_kb,
            dqb_valid: QuotaValid::BLIMITS.bits(),
            ..Default::default()
        };

        self.set_quota_internal(dqblk)
    }

    /// Set inode (file count) soft and hard limits for this id.
    ///
    /// # Arguments
    /// * `soft_limit_count` - Soft limit (number of inodes)
    /// * `hard_limit_count` - Hard limit (number of inodes)
    ///
    /// # Errors
    /// Returns `Error::QuotaOpFailed` if the `quotactl` `set_quota` system call fails.
    pub fn set_inode_limits(&self, soft_limit_count: u64, hard_limit_count: u64) -> Result<()> {
        let dqblk = DqBlk {
            dqb_isoftlimit: soft_limit_count,
            dqb_ihardlimit: hard_limit_count,
            dqb_valid: QuotaValid::ILIMITS.bits(),
            ..Default::default()
        };

        self.set_quota_internal(dqblk)
    }

    /// Set the block btime (soft/hard limit exceedance timer) for this id.
    ///
    /// # Arguments
    /// * `duration` - Duration for the btime timer
    ///
    /// # Errors
    /// Returns `Error::QuotaOpFailed` if the `quotactl` `set_quota` system call fails.
    pub fn set_btime(&self, duration: Duration) -> Result<()> {
        let dqblk = DqBlk {
            dqb_btime: duration.as_secs(),
            dqb_valid: QuotaValid::BTIME.bits(),
            ..Default::default()
        };
        self.set_quota_internal(dqblk)
    }

    /// Set the inode itime (soft/hard limit exceedance timer) for this id.
    ///
    /// # Arguments
    /// * `duration` - Duration for the itime timer
    ///
    /// # Errors
    /// Returns `Error::QuotaOpFailed` if the `quotactl` `set_quota` system call fails.
    pub fn set_itime(&self, duration: Duration) -> Result<()> {
        let dqblk = DqBlk {
            dqb_itime: duration.as_secs(),
            dqb_valid: QuotaValid::ITIME.bits(),
            ..Default::default()
        };
        self.set_quota_internal(dqblk)
    }

    /// Get the block btime (soft/hard limit exceedance timer) for this id.
    ///
    /// # Returns
    /// Duration for the btime timer.
    ///
    /// # Errors
    /// Returns `Error::QuotaOpFailed` if the `quotactl` `get_quota` system call fails.
    pub fn get_btime(&self) -> Result<Duration> {
        let dqblk = self.get_quota()?;
        Ok(Duration::from_secs(dqblk.dqb_btime))
    }

    /// Get the inode itime (soft/hard limit exceedance timer) for this id.
    ///
    /// # Returns
    /// Duration for the itime timer.
    ///
    /// # Errors
    /// Returns `Error::QuotaOpFailed` if the `quotactl` `get_quota` system call fails.
    pub fn get_itime(&self) -> Result<Duration> {
        let dqblk = self.get_quota()?;
        Ok(Duration::from_secs(dqblk.dqb_itime))
    }

    fn get_quota(&self) -> Result<DqBlk> {
        quota::get_quota(self.quota_type, &self.device_path, self.id).map_err(|e| {
            Error::QuotaOpFailed {
                mount_point: self.mount_point.clone(),
                quota_type: self.quota_type,
                id: self.id,
                source: e,
            }
        })
    }

    /// Get the current block soft and hard limits (in kilobytes) for this id.
    ///
    /// # Returns
    /// Tuple of (`soft_limit_kb`, `hard_limit_kb`).
    ///
    /// # Errors
    /// Returns `Error::QuotaOpFailed` if the `quotactl` `get_quota` system call fails.
    pub fn get_block_limit(&self) -> Result<(u64, u64)> {
        let dqblk = self.get_quota()?;
        Ok((dqblk.dqb_bsoftlimit, dqblk.dqb_bhardlimit))
    }

    /// Get the current inode soft and hard limits for this id.
    ///
    /// # Returns
    /// Tuple of (`soft_limit_count`, `hard_limit_count`).
    ///
    /// # Errors
    /// Returns `Error::QuotaOpFailed` if the `quotactl` `get_quota` system call fails.
    pub fn get_inode_limit(&self) -> Result<(u64, u64)> {
        let dqblk = self.get_quota()?;
        Ok((dqblk.dqb_isoftlimit, dqblk.dqb_ihardlimit))
    }

    /// Get the current block usage (in bytes) for this id.
    ///
    /// # Returns
    /// Current block usage in bytes.
    ///
    /// # Errors
    /// Returns `Error::QuotaOpFailed` if the `quotactl` `get_quota` system call fails.
    pub fn get_curspace(&self) -> Result<u64> {
        let dqblk = self.get_quota()?;
        Ok(dqblk.dqb_curspace)
    }

    /// Get the current inode usage (number of inodes) for this id.
    ///
    /// # Returns
    /// Current inode usage (number of inodes).
    ///
    /// # Errors
    /// Returns `Error::QuotaOpFailed` if the `quotactl` `get_quota` system call fails.
    pub fn get_curinodes(&self) -> Result<u64> {
        let dqblk = self.get_quota()?;
        Ok(dqblk.dqb_curinodes)
    }

    /// Get the id (user/group/project) this entry operates on.
    #[must_use]
    pub fn id(&self) -> i32 {
        self.id
    }

    /// Get the mount point path.
    #[must_use]
    pub fn mount_point(&self) -> &Path {
        &self.mount_point
    }

    /// Get the quota type.
    #[must_use]
    pub fn quota_type(&self) -> QuotaType {
        self.quota_type
    }
}

impl fmt::Display for Ext4QuotaEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.get_quota() {
            Ok(dqblk) => {
                let valid = QuotaValid::from_bits_truncate(dqblk.dqb_valid);
                let (bsoft, bhard) = if valid.contains(QuotaValid::BLIMITS) {
                    (
                        dqblk.dqb_bsoftlimit.to_string(),
                        dqblk.dqb_bhardlimit.to_string(),
                    )
                } else {
                    ("-".to_string(), "-".to_string())
                };
                let (isoft, ihard) = if valid.contains(QuotaValid::ILIMITS) {
                    (
                        dqblk.dqb_isoftlimit.to_string(),
                        dqblk.dqb_ihardlimit.to_string(),
                    )
                } else {
                    ("-".to_string(), "-".to_string())
                };
                write!(
                    f,
                    "Ext4QuotaEntry {{ mount_point: {}, quota_type: {:?}, id: {}, block_soft: {}, block_hard: {}, inode_soft: {}, inode_hard: {} }}",
                    self.mount_point.display(),
                    self.quota_type,
                    self.id,
                    bsoft,
                    bhard,
                    isoft,
                    ihard
                )
            }
            Err(_) => {
                write!(
                    f,
                    "Ext4QuotaEntry {{ mount_point: {}, quota_type: {:?}, id: {}, block_soft: -, block_hard: -, inode_soft: -, inode_hard: - }}",
                    self.mount_point.display(),
                    self.quota_type,
                    self.id
                )
            }
        }
    }
}
