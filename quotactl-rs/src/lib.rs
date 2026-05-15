// Copyright 2025-2026 Hyundai Mobis Co., Ltd.
// SPDX-License-Identifier: Apache-2.0

//! Low-level Rust wrapper for the Linux `quotactl` syscall, providing device-path based APIs for disk quota management.
//!
//! This crate exposes types and functions for manipulating disk quotas on ext4, XFS, and other filesystems.
//! It is intended for use by higher-level libraries or applications that require direct access to quota syscalls.
//!
//! # Features
//! - Safe Rust wrappers for common quota operations (on/off, get/set, sync, info)
//! - Support for general and XFS-specific quota commands and structures
//! - Strongly-typed flags and enums for quota types and validity
//! - Testability via mockable syscall layer (see `set_mock`/`clear_mock`)
//!
//! # Example
//! ```rust
//! use quotactl_rs::{quota, QuotaType};
//! use std::path::Path;
//!
//! let device = Path::new("/dev/sda1");
//! let result = quota::get_quota(QuotaType::User, device, 1000);
//! match result {
//!     Ok(quota) => println!("User quota: {:?}", quota),
//!     Err(e) => eprintln!("Error: {}", e),
//! }
//! ```

use bitflags::bitflags;
use std::ffi::CString;
use std::io;
use std::mem::MaybeUninit;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

/// Supported quota types (user, group, project).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum QuotaType {
    User = libc::USRQUOTA,
    Group = libc::GRPQUOTA,
    Project = 2,
}

impl From<QuotaType> for i32 {
    fn from(qt: QuotaType) -> Self {
        qt as i32
    }
}

/// Low-level wrapper for the `quotactl` syscall. Used internally.
///
/// This function wraps the potentially unsafe `libc::quotactl` syscall
/// but provides a safe interface by handling potential errors and memory safety.
fn raw_quotactl(
    op: i32,
    special: Option<&Path>,
    id: i32,
    addr: *mut libc::c_char,
) -> io::Result<()> {
    #[cfg(test)]
    {
        let mock_fn = MOCK.with(|mock| *mock.borrow());
        if let Some(f) = mock_fn {
            return f(op, special, id, addr);
        }
    }

    let special_cstr = special
        .map(|p| {
            CString::new(p.as_os_str().as_bytes())
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))
        })
        .transpose()?;

    let special_ptr = special_cstr
        .as_ref()
        .map_or(std::ptr::null(), |cs| cs.as_ptr());

    let ret = unsafe { libc::quotactl(op, special_ptr, id, addr) };

    if ret == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// General quota operations (device-path based API).
pub mod quota {
    use super::{CString, MaybeUninit, OsStrExt, Path, QuotaType, bitflags, io, raw_quotactl};

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    #[repr(i32)]
    pub enum QuotaFmt {
        VfsOld = libc::QFMT_VFS_OLD,
        VfsV0 = libc::QFMT_VFS_V0,
        VfsV1 = libc::QFMT_VFS_V1,
    }

    impl From<QuotaFmt> for i32 {
        fn from(fmt: QuotaFmt) -> Self {
            fmt as i32
        }
    }

    impl TryFrom<i32> for QuotaFmt {
        type Error = io::Error;

        fn try_from(fmt: i32) -> Result<Self, Self::Error> {
            match fmt {
                libc::QFMT_VFS_OLD => Ok(QuotaFmt::VfsOld),
                libc::QFMT_VFS_V0 => Ok(QuotaFmt::VfsV0),
                libc::QFMT_VFS_V1 => Ok(QuotaFmt::VfsV1),
                _ => Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Invalid quota format",
                )),
            }
        }
    }

    /// Disk quota block information (limits, usage, times, validity flags).
    #[derive(Debug, Clone, Default, PartialEq, Eq)]
    pub struct DqBlk {
        pub dqb_bhardlimit: u64,
        pub dqb_bsoftlimit: u64,
        pub dqb_curspace: u64,
        pub dqb_ihardlimit: u64,
        pub dqb_isoftlimit: u64,
        pub dqb_curinodes: u64,
        pub dqb_btime: u64,
        pub dqb_itime: u64,
        pub dqb_valid: u32,
    }

    impl From<libc::dqblk> for DqBlk {
        fn from(dq: libc::dqblk) -> Self {
            Self {
                dqb_bhardlimit: dq.dqb_bhardlimit,
                dqb_bsoftlimit: dq.dqb_bsoftlimit,
                dqb_curspace: dq.dqb_curspace,
                dqb_ihardlimit: dq.dqb_ihardlimit,
                dqb_isoftlimit: dq.dqb_isoftlimit,
                dqb_curinodes: dq.dqb_curinodes,
                dqb_btime: dq.dqb_btime,
                dqb_itime: dq.dqb_itime,
                dqb_valid: dq.dqb_valid,
            }
        }
    }

    impl From<DqBlk> for libc::dqblk {
        fn from(dq: DqBlk) -> Self {
            let mut dqblk: libc::dqblk = unsafe { MaybeUninit::zeroed().assume_init() };
            dqblk.dqb_bhardlimit = dq.dqb_bhardlimit;
            dqblk.dqb_bsoftlimit = dq.dqb_bsoftlimit;
            dqblk.dqb_curspace = dq.dqb_curspace;
            dqblk.dqb_ihardlimit = dq.dqb_ihardlimit;
            dqblk.dqb_isoftlimit = dq.dqb_isoftlimit;
            dqblk.dqb_curinodes = dq.dqb_curinodes;
            dqblk.dqb_btime = dq.dqb_btime;
            dqblk.dqb_itime = dq.dqb_itime;
            dqblk.dqb_valid = dq.dqb_valid;
            dqblk
        }
    }

    /// Generic quota information (grace times, flags, validity).
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    pub struct DqInfo {
        pub dqi_bgrace: u64,
        pub dqi_igrace: u64,
        pub dqi_flags: u32,
        pub dqi_valid: u32,
    }

    bitflags! {
        /// Flags for DqInfo (e.g., SYS_FILE, ROOT_SQUASH).
        #[derive(Debug)]
        pub struct DqInfoFlags: u32 {
            const SYS_FILE = 0x10000;
            const ROOT_SQUASH = 0x20000;
        }
    }

    bitflags! {
        /// Validity flags for DqBlk fields.
        pub struct QuotaValid: u32 {
            const BLIMITS = libc::QIF_BLIMITS;
            const SPACE   = libc::QIF_SPACE;
            const ILIMITS = libc::QIF_ILIMITS;
            const INODES  = libc::QIF_INODES;
            const BTIME   = libc::QIF_BTIME;
            const ITIME   = libc::QIF_ITIME;
            const LIMITS  = libc::QIF_LIMITS;
            const USAGE   = libc::QIF_USAGE;
            const TIMES   = libc::QIF_TIMES;
            const ALL     = libc::QIF_ALL;
        }
    }

    bitflags! {
        /// Validity flags for DqInfo fields.
        #[derive(Debug)]
        pub struct InfoValid: u32 {
            const BGRACE = 1;
            const IGRACE = 2;
            const FLAGS = 4;
            const ALL = Self::BGRACE.bits() | Self::IGRACE.bits() | Self::FLAGS.bits();
        }
    }

    /// Kernel dqinfo struct (internal, C layout, always zeroed on creation).
    #[allow(non_camel_case_types)]
    #[repr(C)]
    #[derive(Debug, Clone, Copy)]
    pub struct dqinfo {
        pub dqi_bgrace: u64,
        pub dqi_igrace: u64,
        pub dqi_flags: u32,
        pub dqi_valid: u32,
    }

    impl Default for dqinfo {
        fn default() -> Self {
            unsafe { MaybeUninit::zeroed().assume_init() }
        }
    }

    impl From<DqInfo> for dqinfo {
        fn from(info: DqInfo) -> Self {
            dqinfo {
                dqi_bgrace: info.dqi_bgrace,
                dqi_igrace: info.dqi_igrace,
                dqi_flags: info.dqi_flags,
                dqi_valid: info.dqi_valid,
            }
        }
    }

    impl From<dqinfo> for DqInfo {
        fn from(dq: dqinfo) -> Self {
            DqInfo {
                dqi_bgrace: dq.dqi_bgrace,
                dqi_igrace: dq.dqi_igrace,
                dqi_flags: dq.dqi_flags,
                dqi_valid: dq.dqi_valid,
            }
        }
    }

    /// Enable quota enforcement for a device and quota type.
    ///
    /// # Errors
    ///
    /// Returns an `io::Error` if the underlying `quotactl` syscall fails.
    pub fn quota_on(
        quota_type: QuotaType,
        special: &Path, // Device path
        format: QuotaFmt,
        quota_file: Option<&Path>,
    ) -> io::Result<()> {
        let op = libc::QCMD(libc::Q_QUOTAON, quota_type.into());

        let cstr_opt = quota_file
            .map(|path| {
                CString::new(path.as_os_str().as_bytes())
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))
            })
            .transpose()?;

        let addr = cstr_opt
            .as_ref()
            .map_or(std::ptr::null_mut(), |cstr| cstr.as_ptr().cast_mut());

        raw_quotactl(op, Some(special), format.into(), addr)
    }

    /// Disable quota enforcement for a device and quota type.
    ///
    /// # Errors
    ///
    /// Returns an `io::Error` if the underlying `quotactl` syscall fails.
    pub fn quota_off(quota_type: QuotaType, special: &Path) -> io::Result<()> {
        let op = libc::QCMD(libc::Q_QUOTAOFF, quota_type.into());
        raw_quotactl(op, Some(special), 0, std::ptr::null_mut())
    }

    /// Get quota block info for a given id.
    ///
    /// # Errors
    ///
    /// Returns an `io::Error` if the underlying `quotactl` syscall fails.
    pub fn get_quota(quota_type: QuotaType, special: &Path, id: i32) -> io::Result<DqBlk> {
        let op = libc::QCMD(libc::Q_GETQUOTA, quota_type.into());
        let mut dqblk_libc: libc::dqblk = unsafe { MaybeUninit::zeroed().assume_init() };
        let addr = (&raw mut dqblk_libc).cast::<libc::c_char>();
        raw_quotactl(op, Some(special), id, addr)?;
        Ok(dqblk_libc.into())
    }

    /// Set quota block info for a given id.
    ///
    /// # Errors
    ///
    /// Returns an `io::Error` if the underlying `quotactl` syscall fails.
    pub fn set_quota(
        quota_type: QuotaType,
        special: &Path,
        id: i32,
        dqblk: DqBlk,
    ) -> io::Result<()> {
        let op = libc::QCMD(libc::Q_SETQUOTA, quota_type.into());
        let mut dqblk_libc: libc::dqblk = dqblk.into();
        let addr_mut = (&raw mut dqblk_libc).cast::<libc::c_char>();
        raw_quotactl(op, Some(special), id, addr_mut)
    }

    /// Sync quota usage to disk for a device/quota type.
    ///
    /// If `special` is `None`, quotas are synced for all filesystems where quotas are active for the given `quota_type`.
    ///
    /// # Errors
    ///
    /// Returns an `io::Error` if the underlying `quotactl` syscall fails.
    pub fn sync_quotas(
        special: Option<&Path>, // Optional device path
        quota_type: QuotaType,
    ) -> io::Result<()> {
        let op = libc::QCMD(libc::Q_SYNC, quota_type.into());
        raw_quotactl(op, special, 0, std::ptr::null_mut())
    }

    /// Get generic quota info (grace times, flags).
    ///
    /// # Errors
    ///
    /// Returns an `io::Error` if the underlying `quotactl` syscall fails.
    pub fn get_info(quota_type: QuotaType, special: &Path) -> io::Result<DqInfo> {
        let op = libc::QCMD(libc::Q_GETINFO, quota_type.into());
        let mut dqinfo_local: dqinfo = dqinfo::default();
        let addr = (&raw mut dqinfo_local).cast::<libc::c_char>();
        raw_quotactl(op, Some(special), 0, addr)?;
        Ok(dqinfo_local.into())
    }

    /// Set generic quota info (grace times, flags).
    ///
    /// # Errors
    ///
    /// Returns an `io::Error` if the underlying `quotactl` syscall fails.
    pub fn set_info(quota_type: QuotaType, special: &Path, dqinfo: DqInfo) -> io::Result<()> {
        let op = libc::QCMD(libc::Q_SETINFO, quota_type.into());
        let mut dqinfo_to_set: dqinfo = dqinfo.into();
        let addr = (&raw mut dqinfo_to_set).cast::<libc::c_char>();
        raw_quotactl(op, Some(special), 0, addr)
    }

    /// Get quota format for a device/quota type.
    ///
    /// # Errors
    ///
    /// Returns an `io::Error` if the underlying `quotactl` syscall fails.
    pub fn get_fmt(quota_type: QuotaType, special: &Path) -> io::Result<QuotaFmt> {
        let op = libc::QCMD(libc::Q_GETFMT, quota_type.into());
        let mut format: i32 = 0;
        let addr = (&raw mut format).cast::<libc::c_char>();
        raw_quotactl(op, Some(special), 0, addr)?;
        QuotaFmt::try_from(format)
    }
}

/// XFS-specific quota operations and types.
pub mod xfs_quota {
    use super::{MaybeUninit, Path, QuotaType, bitflags, io, raw_quotactl};

    impl From<QuotaType> for FsQuotaStateFlags {
        fn from(value: QuotaType) -> Self {
            match value {
                QuotaType::User => Self::FS_QUOTA_UDQ_ENFD,
                QuotaType::Group => Self::FS_QUOTA_GDQ_ENFD,
                QuotaType::Project => Self::FS_QUOTA_PDQ_ENFD,
            }
        }
    }

    // --- XFS Specific Constants ---
    // Command constructor macro (Rust equivalent)
    const fn xqm_cmd(cmd: i32) -> i32 {
        (('X' as i32) << 8) + cmd
    }

    const FS_DQUOT_VERSION: i8 = 1;

    // XFS quotactl commands
    pub(crate) const Q_XQUOTAON: i32 = xqm_cmd(1);
    pub(crate) const Q_XQUOTAOFF: i32 = xqm_cmd(2);
    pub(crate) const Q_XGETQUOTA: i32 = xqm_cmd(3);
    pub(crate) const Q_XSETQLIM: i32 = xqm_cmd(4);
    pub(crate) const Q_XGETQSTAT: i32 = xqm_cmd(5);
    pub(crate) const Q_XQUOTARM: i32 = xqm_cmd(6);
    pub(crate) const Q_XQUOTASYNC: i32 = xqm_cmd(7);
    pub(crate) const Q_XGETQSTATV: i32 = xqm_cmd(8);
    pub(crate) const Q_XGETNEXTQUOTA: i32 = xqm_cmd(9);

    bitflags! {
        /// Flags for FS quota types (user, group, project).
        #[repr(C)]
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub struct DqFlags: u8 {
            const FS_USER_QUOTA = 1 << 0;
            const FS_PROJ_QUOTA = 1 << 1;
            const FS_GROUP_QUOTA = 1 << 2;
        }
    }

    bitflags! {
        /// Field mask flags for FS quota fields.
        #[repr(C)]
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub struct DqFieldMask: u16 {
            const FS_DQ_ISOFT    = 1 << 0;
            const FS_DQ_IHARD    = 1 << 1;
            const FS_DQ_BSOFT    = 1 << 2;
            const FS_DQ_BHARD    = 1 << 3;
            const FS_DQ_RTBSOFT  = 1 << 4;
            const FS_DQ_RTBHARD  = 1 << 5;
            const FS_DQ_BTIMER   = 1 << 6;
            const FS_DQ_ITIMER   = 1 << 7;
            const FS_DQ_RTBTIMER = 1 << 8;
            const FS_DQ_BWARNS   = 1 << 9;
            const FS_DQ_IWARNS   = 1 << 10;
            const FS_DQ_RTBWARNS = 1 << 11;
            const FS_DQ_BCOUNT   = 1 << 12;
            const FS_DQ_ICOUNT   = 1 << 13;
            const FS_DQ_RTBCOUNT = 1 << 14;
            const FS_DQ_BIGTIME  = 1 << 15;
        }
    }

    bitflags! {
        /// Flags for FS quota state (accounting, enforcement).
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub struct FsQuotaStateFlags: u32 {
            const FS_QUOTA_UDQ_ACCT = 1 << 0;
            const FS_QUOTA_UDQ_ENFD = 1 << 1;
            const FS_QUOTA_GDQ_ACCT = 1 << 2;
            const FS_QUOTA_GDQ_ENFD = 1 << 3;
            const FS_QUOTA_PDQ_ACCT = 1 << 4;
            const FS_QUOTA_PDQ_ENFD = 1 << 5;
        }
    }

    // --- FS Specific Structs (Internal C representations) ---
    #[repr(C)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    #[allow(
        clippy::struct_field_names,
        reason = "XFS kernel struct: field names mirror the C ABI and cannot be changed"
    )]
    #[allow(non_camel_case_types)]
    pub(crate) struct fs_qfilestat {
        pub qfs_ino: u64,
        pub qfs_nblks: u64,
        pub qfs_nextents: u32,
    }

    impl Default for fs_qfilestat {
        fn default() -> Self {
            unsafe { MaybeUninit::zeroed().assume_init() }
        }
    }

    #[repr(C)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    #[allow(
        clippy::struct_field_names,
        reason = "XFS kernel struct: field names mirror the C ABI and cannot be changed"
    )]
    #[allow(non_camel_case_types)]
    pub(crate) struct fs_qfilestatv {
        pub qfs_ino: u64,
        pub qfs_nblks: u64,
        pub qfs_nextents: u32,
        pub qfs_pad: u32,
    }

    impl Default for fs_qfilestatv {
        fn default() -> Self {
            unsafe { MaybeUninit::zeroed().assume_init() }
        }
    }

    #[repr(C)]
    #[derive(Debug, Clone, Copy)]
    #[allow(
        clippy::struct_field_names,
        reason = "XFS kernel struct: field names mirror the C ABI and cannot be changed"
    )]
    #[allow(non_camel_case_types)]
    pub(crate) struct fs_quota_stat {
        pub qs_version: i8,
        pub qs_flags: u16,
        pub qs_pad: i8,
        pub qs_uquota: fs_qfilestat,
        pub qs_gquota: fs_qfilestat,
        pub qs_incoredqs: u32,
        pub qs_btimelimit: i32,
        pub qs_itimelimit: i32,
        pub qs_rtbtimelimit: i32,
        pub qs_bwarnlimit: u16,
        pub qs_iwarnlimit: u16,
    }

    impl Default for fs_quota_stat {
        fn default() -> Self {
            unsafe { MaybeUninit::zeroed().assume_init() }
        }
    }

    #[repr(C)]
    #[derive(Debug, Clone, Copy)]
    #[allow(
        clippy::struct_field_names,
        reason = "XFS kernel struct: field names mirror the C ABI and cannot be changed"
    )]
    #[allow(non_camel_case_types)]
    pub(crate) struct fs_quota_statv {
        pub qs_version: i8,
        pub qs_pad1: u8,
        pub qs_flags: u16,
        pub qs_incoredqs: u32,
        pub qs_uquota: fs_qfilestatv,
        pub qs_gquota: fs_qfilestatv,
        pub qs_pquota: fs_qfilestatv,
        pub qs_btimelimit: i32,
        pub qs_itimelimit: i32,
        pub qs_rtbtimelimit: i32,
        pub qs_bwarnlimit: u16,
        pub qs_iwarnlimit: u16,
        pub qs_rtbwarnlimit: u16,
        pub qs_pad3: u16,
        pub qs_pad4: u32,
        pub qs_pad2: [u64; 7],
    }

    impl Default for fs_quota_statv {
        fn default() -> Self {
            unsafe { MaybeUninit::zeroed().assume_init() }
        }
    }

    /// FS disk quota structure (C layout, internal).
    #[repr(C)]
    #[derive(Debug, Clone, Copy)]
    #[allow(
        clippy::struct_field_names,
        reason = "XFS kernel struct: field names mirror the C ABI and cannot be changed"
    )]
    #[allow(non_camel_case_types)]
    struct fs_disk_quota {
        pub d_version: i8,
        pub d_flags: u8,
        pub d_fieldmask: u16,
        pub d_id: u32,
        pub d_blk_hardlimit: u64,
        pub d_blk_softlimit: u64,
        pub d_ino_hardlimit: u64,
        pub d_ino_softlimit: u64,
        pub d_bcount: u64,
        pub d_icount: u64,
        pub d_itimer: i32,
        pub d_btimer: i32,
        pub d_iwarns: u16,
        pub d_bwarns: u16,
        pub d_itimer_hi: i8,
        pub d_btimer_hi: i8,
        pub d_rtbtimer_hi: i8,
        pub d_padding2: i8,
        pub d_rtb_hardlimit: u64,
        pub d_rtb_softlimit: u64,
        pub d_rtbcount: u64,
        pub d_rtbtimer: i32,
        pub d_rtbwarns: u16,
        pub d_padding3: i16,
        pub d_padding4: [u8; 8],
    }

    impl Default for fs_disk_quota {
        fn default() -> Self {
            unsafe { MaybeUninit::zeroed().assume_init() }
        }
    }

    /// Public FS disk quota structure (safe Rust type).
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct FsDiskQuota {
        pub version: i8,
        pub flags: DqFlags,
        pub fieldmask: DqFieldMask,
        pub id: u32,
        pub blk_hardlimit: u64,
        pub blk_softlimit: u64,
        pub ino_hardlimit: u64,
        pub ino_softlimit: u64,
        pub bcount: u64,
        pub icount: u64,
        pub itimer: i64,
        pub btimer: i64,
        pub iwarns: u16,
        pub bwarns: u16,
        pub rtb_hardlimit: u64,
        pub rtb_softlimit: u64,
        pub rtbcount: u64,
        pub rtbtimer: i64,
        pub rtbwarns: u16,
    }

    impl Default for FsDiskQuota {
        fn default() -> Self {
            Self {
                version: FS_DQUOT_VERSION,
                flags: DqFlags::empty(),
                fieldmask: DqFieldMask::empty(),
                id: 0,
                blk_hardlimit: 0,
                blk_softlimit: 0,
                ino_hardlimit: 0,
                ino_softlimit: 0,
                bcount: 0,
                icount: 0,
                itimer: 0,
                btimer: 0,
                iwarns: 0,
                bwarns: 0,
                rtb_hardlimit: 0,
                rtb_softlimit: 0,
                rtbcount: 0,
                rtbtimer: 0,
                rtbwarns: 0,
            }
        }
    }

    #[allow(
        clippy::cast_sign_loss,
        reason = "Intentional bit-level reinterpretation of i32 timer low word as u32 before widening to i64 per XFS kernel protocol"
    )]
    fn combine_timer(low: i32, high: i8) -> i64 {
        ((i64::from(high)) << 32) | i64::from(low as u32)
    }

    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_possible_wrap,
        reason = "Intentional bit-level split of XFS timer: low 32 bits and high 8 bits per kernel protocol"
    )]
    fn split_timer(timer: i64) -> (i32, i8) {
        let low = timer as i32;
        let high = (timer >> 32) as i8;
        (low, high)
    }

    impl From<fs_disk_quota> for FsDiskQuota {
        fn from(dq: fs_disk_quota) -> Self {
            FsDiskQuota {
                version: dq.d_version,
                flags: DqFlags::from_bits_retain(dq.d_flags),
                fieldmask: DqFieldMask::from_bits_retain(dq.d_fieldmask),
                id: dq.d_id,
                blk_hardlimit: dq.d_blk_hardlimit,
                blk_softlimit: dq.d_blk_softlimit,
                ino_hardlimit: dq.d_ino_hardlimit,
                ino_softlimit: dq.d_ino_softlimit,
                bcount: dq.d_bcount,
                icount: dq.d_icount,
                itimer: combine_timer(dq.d_itimer, dq.d_itimer_hi),
                btimer: combine_timer(dq.d_btimer, dq.d_btimer_hi),
                iwarns: dq.d_iwarns,
                bwarns: dq.d_bwarns,
                rtb_hardlimit: dq.d_rtb_hardlimit,
                rtb_softlimit: dq.d_rtb_softlimit,
                rtbcount: dq.d_rtbcount,
                rtbtimer: combine_timer(dq.d_rtbtimer, dq.d_rtbtimer_hi),
                rtbwarns: dq.d_rtbwarns,
            }
        }
    }

    impl From<FsDiskQuota> for fs_disk_quota {
        fn from(info: FsDiskQuota) -> Self {
            let (itimer_low, itimer_hi) = split_timer(info.itimer);
            let (btimer_low, btimer_hi) = split_timer(info.btimer);
            let (rtbtimer_low, rtbtimer_hi) = split_timer(info.rtbtimer);
            fs_disk_quota {
                d_version: info.version,
                d_flags: info.flags.bits(),
                d_fieldmask: info.fieldmask.bits(),
                d_id: info.id,
                d_blk_hardlimit: info.blk_hardlimit,
                d_blk_softlimit: info.blk_softlimit,
                d_ino_hardlimit: info.ino_hardlimit,
                d_ino_softlimit: info.ino_softlimit,
                d_bcount: info.bcount,
                d_icount: info.icount,
                d_itimer: itimer_low,
                d_btimer: btimer_low,
                d_iwarns: info.iwarns,
                d_bwarns: info.bwarns,
                d_itimer_hi: itimer_hi,
                d_btimer_hi: btimer_hi,
                d_rtbtimer_hi: rtbtimer_hi,
                d_padding2: 0,
                d_rtb_hardlimit: info.rtb_hardlimit,
                d_rtb_softlimit: info.rtb_softlimit,
                d_rtbcount: info.rtbcount,
                d_rtbtimer: rtbtimer_low,
                d_rtbwarns: info.rtbwarns,
                d_padding3: 0,
                d_padding4: [0; 8],
            }
        }
    }

    /// FS quota file statistics (basic information).
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    pub struct FsQuotaFileStat {
        pub ino: u64,
        pub nblks: u64,
        pub nextents: u32,
    }

    impl From<fs_qfilestat> for FsQuotaFileStat {
        fn from(fs: fs_qfilestat) -> Self {
            Self {
                ino: fs.qfs_ino,
                nblks: fs.qfs_nblks,
                nextents: fs.qfs_nextents,
            }
        }
    }

    /// FS quota status (basic information).
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct FsQuotaStat {
        pub version: i8,
        pub flags: FsQuotaStateFlags,
        pub uquota: FsQuotaFileStat,
        pub gquota: FsQuotaFileStat,
        pub incoredqs: u32,
        pub btimelimit: i32,
        pub itimelimit: i32,
        pub rtbtimelimit: i32,
        pub bwarnlimit: u16,
        pub iwarnlimit: u16,
    }

    impl Default for FsQuotaStat {
        fn default() -> Self {
            Self {
                version: 0,
                flags: FsQuotaStateFlags::empty(),
                uquota: FsQuotaFileStat::default(),
                gquota: FsQuotaFileStat::default(),
                incoredqs: 0,
                btimelimit: 0,
                itimelimit: 0,
                rtbtimelimit: 0,
                bwarnlimit: 0,
                iwarnlimit: 0,
            }
        }
    }

    impl From<fs_quota_stat> for FsQuotaStat {
        fn from(qs: fs_quota_stat) -> Self {
            Self {
                version: qs.qs_version,
                flags: FsQuotaStateFlags::from_bits_retain(u32::from(qs.qs_flags)),
                uquota: qs.qs_uquota.into(),
                gquota: qs.qs_gquota.into(),
                incoredqs: qs.qs_incoredqs,
                btimelimit: qs.qs_btimelimit,
                itimelimit: qs.qs_itimelimit,
                rtbtimelimit: qs.qs_rtbtimelimit,
                bwarnlimit: qs.qs_bwarnlimit,
                iwarnlimit: qs.qs_iwarnlimit,
            }
        }
    }

    /// Extended FS quota file statistics (additional information).
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    pub struct FsQuotaFileStatV {
        pub ino: u64,
        pub nblks: u64,
        pub nextents: u32,
    }

    impl From<fs_qfilestatv> for FsQuotaFileStatV {
        fn from(fs: fs_qfilestatv) -> Self {
            Self {
                ino: fs.qfs_ino,
                nblks: fs.qfs_nblks,
                nextents: fs.qfs_nextents,
            }
        }
    }

    /// Extended FS quota status (additional information).
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct FsQuotaStatV {
        pub version: i8,
        pub flags: FsQuotaStateFlags,
        pub incoredqs: u32,
        pub uquota: FsQuotaFileStatV,
        pub gquota: FsQuotaFileStatV,
        pub pquota: FsQuotaFileStatV,
        pub btimelimit: i32,
        pub itimelimit: i32,
        pub rtbtimelimit: i32,
        pub bwarnlimit: u16,
        pub iwarnlimit: u16,
        pub rtbwarnlimit: u16,
    }

    impl Default for FsQuotaStatV {
        fn default() -> Self {
            Self {
                version: 0,
                flags: FsQuotaStateFlags::empty(),
                incoredqs: 0,
                uquota: FsQuotaFileStatV::default(),
                gquota: FsQuotaFileStatV::default(),
                pquota: FsQuotaFileStatV::default(),
                btimelimit: 0,
                itimelimit: 0,
                rtbtimelimit: 0,
                bwarnlimit: 0,
                iwarnlimit: 0,
                rtbwarnlimit: 0,
            }
        }
    }

    impl From<fs_quota_statv> for FsQuotaStatV {
        fn from(qs: fs_quota_statv) -> Self {
            Self {
                version: qs.qs_version,
                flags: FsQuotaStateFlags::from_bits_retain(u32::from(qs.qs_flags)),
                incoredqs: qs.qs_incoredqs,
                uquota: qs.qs_uquota.into(),
                gquota: qs.qs_gquota.into(),
                pquota: qs.qs_pquota.into(),
                btimelimit: qs.qs_btimelimit,
                itimelimit: qs.qs_itimelimit,
                rtbtimelimit: qs.qs_rtbtimelimit,
                bwarnlimit: qs.qs_bwarnlimit,
                iwarnlimit: qs.qs_iwarnlimit,
                rtbwarnlimit: qs.qs_rtbwarnlimit,
            }
        }
    }

    /// Enable XFS quota enforcement for a device and quota type.
    ///
    /// # Errors
    ///
    /// Returns an `io::Error` if the underlying `quotactl` syscall fails.
    pub fn x_quota_on(quota_type: QuotaType, special: &Path) -> io::Result<()> {
        let flags: FsQuotaStateFlags = quota_type.into();
        let mut flags_val = flags.bits();
        let addr = (&raw mut flags_val).cast::<libc::c_char>();
        raw_quotactl(
            libc::QCMD(Q_XQUOTAON, quota_type.into()),
            Some(special),
            0,
            addr,
        )
    }

    /// Disable XFS quota enforcement for a device and quota type.
    ///
    /// # Errors
    ///
    /// Returns an `io::Error` if the underlying `quotactl` syscall fails.
    pub fn x_quota_off(quota_type: QuotaType, special: &Path) -> io::Result<()> {
        let flags: FsQuotaStateFlags = quota_type.into();
        let mut flags_val = flags.bits();
        let addr = (&raw mut flags_val).cast::<libc::c_char>();
        raw_quotactl(
            libc::QCMD(Q_XQUOTAOFF, quota_type.into()),
            Some(special),
            0,
            addr,
        )
    }

    /// Get XFS quota info for a given id.
    ///
    /// # Errors
    ///
    /// Returns an `io::Error` if the underlying `quotactl` syscall fails.
    pub fn x_get_quota(quota_type: QuotaType, special: &Path, id: u32) -> io::Result<FsDiskQuota> {
        let op = libc::QCMD(Q_XGETQUOTA, quota_type.into());
        let mut dq_blk = fs_disk_quota::default();
        let addr = (&raw mut dq_blk).cast::<libc::c_char>();
        let id_i32 = i32::from_ne_bytes(id.to_ne_bytes());
        raw_quotactl(op, Some(special), id_i32, addr)?;
        Ok(dq_blk.into())
    }

    /// Set XFS quota limits for a given id.
    ///
    /// # Errors
    ///
    /// Returns an `io::Error` if the underlying `quotactl` syscall fails.
    pub fn x_set_qlim(
        quota_type: QuotaType,
        special: &Path,
        id: u32,
        dqblk: FsDiskQuota,
    ) -> io::Result<()> {
        let op = libc::QCMD(Q_XSETQLIM, quota_type.into());
        let mut raw_dq: fs_disk_quota = dqblk.into();
        let addr = (&raw mut raw_dq).cast::<libc::c_char>();
        let id_i32 = i32::from_ne_bytes(id.to_ne_bytes());
        raw_quotactl(op, Some(special), id_i32, addr)
    }

    /// Get XFS quota status for a device and specific quota type.
    ///
    /// # Errors
    ///
    /// Returns an `io::Error` if the underlying `quotactl` syscall fails.
    pub fn x_get_qstat(quota_type: QuotaType, special: &Path) -> io::Result<FsQuotaStat> {
        let mut qstat = fs_quota_stat::default();
        let addr = (&raw mut qstat).cast::<libc::c_char>();
        let op = libc::QCMD(Q_XGETQSTAT, quota_type.into());
        raw_quotactl(op, Some(special), 0, addr)?;
        Ok(qstat.into())
    }

    /// Remove XFS quota for a device.
    ///
    /// Note: Quotas must have already been turned off for the specified type before calling this function.
    ///
    /// # Errors
    ///
    /// Returns an `io::Error` if the underlying `quotactl` syscall fails.
    pub fn x_quota_rm(quota_type: QuotaType, special: &Path) -> io::Result<()> {
        let op = libc::QCMD(Q_XQUOTARM, quota_type.into());
        let flags = match quota_type {
            QuotaType::User => DqFlags::FS_USER_QUOTA,
            QuotaType::Group => DqFlags::FS_GROUP_QUOTA,
            QuotaType::Project => DqFlags::FS_PROJ_QUOTA,
        };
        let mut flags_val: u16 = u16::from(flags.bits());
        let addr = (&raw mut flags_val).cast::<libc::c_char>();

        raw_quotactl(op, Some(special), 0, addr)
    }

    /// Sync XFS quota usage to disk.
    ///
    /// Note: Since Linux 3.4, this operation is generally a no-op as XFS syncs quotas automatically.
    ///
    /// # Errors
    ///
    /// Returns an `io::Error` if the underlying `quotactl` syscall fails.
    pub fn x_quota_sync(quota_type: QuotaType) -> io::Result<()> {
        let op = libc::QCMD(Q_XQUOTASYNC, quota_type.into());
        // special, id, and addr are ignored for Q_XQUOTASYNC.
        raw_quotactl(op, None, 0, std::ptr::null_mut())
    }

    /// Get extended XFS quota status for a device and quota type.
    ///
    /// # Errors
    ///
    /// Returns an `io::Error` if the underlying `quotactl` syscall fails.
    pub fn x_get_qstatv(quota_type: QuotaType, special: &Path) -> io::Result<FsQuotaStatV> {
        let mut qstatv = fs_quota_statv {
            qs_version: 1,
            ..fs_quota_statv::default()
        };
        let addr = (&raw mut qstatv).cast::<libc::c_char>();
        let op = libc::QCMD(Q_XGETQSTATV, quota_type.into());
        raw_quotactl(op, Some(special), 0, addr)?;
        Ok(qstatv.into())
    }

    /// Get next XFS quota record for a given id.
    ///
    /// # Errors
    ///
    /// Returns an `io::Error` if the underlying `quotactl` syscall fails.
    pub fn x_get_next_quota(
        quota_type: QuotaType,
        special: &Path,
        id: u32,
    ) -> io::Result<FsDiskQuota> {
        let op = libc::QCMD(Q_XGETNEXTQUOTA, quota_type.into());
        let mut dq_xfs: fs_disk_quota = fs_disk_quota::default();
        let addr = (&raw mut dq_xfs).cast::<libc::c_char>();
        let id_i32 = i32::from_ne_bytes(id.to_ne_bytes());
        raw_quotactl(op, Some(special), id_i32, addr)?;
        Ok(dq_xfs.into())
    }
}

#[cfg(test)]
use std::cell::RefCell;

#[cfg(test)]
type MockFn = fn(i32, Option<&Path>, i32, *mut libc::c_char) -> io::Result<()>;

#[cfg(test)]
thread_local! {
    pub static MOCK: RefCell<Option<MockFn>> = RefCell::new(None);
}

#[cfg(test)]
/// Set a mock function for testing `raw_quotactl` calls.
pub fn set_mock(f: MockFn) {
    MOCK.with(|mock| *mock.borrow_mut() = Some(f));
}

#[cfg(test)]
/// Clear the mock function for `raw_quotactl` calls.
pub fn clear_mock() {
    MOCK.with(|mock| *mock.borrow_mut() = None);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::quota::{DqBlk, DqInfo, QuotaFmt};
    use std::path::Path;

    #[test]
    fn test_quota_on() {
        set_mock(|op, special, id, _addr| {
            assert_eq!(op, libc::QCMD(libc::Q_QUOTAON, QuotaType::User.into()));
            assert_eq!(id, libc::QFMT_VFS_V0);
            assert_eq!(special.unwrap(), Path::new("/dev/loop999"));
            Ok(())
        });
        let path = Path::new("/dev/loop999");
        let res = quota::quota_on(QuotaType::User, path, QuotaFmt::VfsV0, None);
        assert!(res.is_ok());
        clear_mock();
    }

    #[test]
    fn test_quota_off() {
        set_mock(|op, special, id, _addr| {
            assert_eq!(op, libc::QCMD(libc::Q_QUOTAOFF, QuotaType::Group.into()));
            assert_eq!(id, 0);
            assert_eq!(special.unwrap(), Path::new("/dev/loop999"));
            Ok(())
        });
        let path = Path::new("/dev/loop999");
        let res = quota::quota_off(QuotaType::Group, path);
        assert!(res.is_ok());
        clear_mock();
    }

    #[test]
    fn test_get_quota() {
        set_mock(|op, special, id, _addr| {
            assert_eq!(op, libc::QCMD(libc::Q_GETQUOTA, QuotaType::User.into()));
            assert_eq!(id, 42);
            assert_eq!(special.unwrap(), Path::new("/dev/loop999"));
            Ok(())
        });
        let path = Path::new("/dev/loop999");
        let res = quota::get_quota(QuotaType::User, path, 42);
        assert!(res.is_ok());
        clear_mock();
    }

    #[test]
    fn test_set_quota() {
        set_mock(|op, special, id, _addr| {
            assert_eq!(op, libc::QCMD(libc::Q_SETQUOTA, QuotaType::User.into()));
            assert_eq!(id, 77);
            assert_eq!(special.unwrap(), Path::new("/dev/loop999"));
            Ok(())
        });
        let path = Path::new("/dev/loop999");
        let dqblk = DqBlk {
            dqb_valid: 123,
            ..Default::default()
        };
        let res = quota::set_quota(QuotaType::User, path, 77, dqblk);
        assert!(res.is_ok());
        clear_mock();
    }

    #[test]
    fn test_sync_quotas() {
        set_mock(|op, special, id, _addr| {
            assert_eq!(op, libc::QCMD(libc::Q_SYNC, QuotaType::Project.into()));
            assert_eq!(id, 0);
            assert_eq!(special.unwrap(), Path::new("/dev/loop999"));
            Ok(())
        });
        let path = Path::new("/dev/loop999");
        let res = quota::sync_quotas(Some(path), QuotaType::Project);
        assert!(res.is_ok());
        clear_mock();
    }

    #[test]
    fn test_get_info() {
        set_mock(|op, special, id, _addr| {
            assert_eq!(op, libc::QCMD(libc::Q_GETINFO, QuotaType::User.into()));
            assert_eq!(id, 0);
            assert_eq!(special.unwrap(), Path::new("/dev/loop999"));
            Ok(())
        });
        let path = Path::new("/dev/loop999");
        let res = quota::get_info(QuotaType::User, path);
        assert!(res.is_ok());
        clear_mock();
    }

    #[test]
    fn test_set_info() {
        set_mock(|op, special, id, _addr| {
            assert_eq!(op, libc::QCMD(libc::Q_SETINFO, QuotaType::User.into()));
            assert_eq!(id, 0);
            assert_eq!(special.unwrap(), Path::new("/dev/loop999"));
            Ok(())
        });
        let path = Path::new("/dev/loop999");
        let dqinfo = DqInfo {
            dqi_valid: 1,
            ..Default::default()
        };
        let res = quota::set_info(QuotaType::User, path, dqinfo);
        assert!(res.is_ok());
        clear_mock();
    }

    #[test]
    #[allow(
        clippy::cast_ptr_alignment,
        reason = "Test-only mock: addr is *mut c_char from mock syscall; actual alignment guaranteed by calling code"
    )]
    fn test_get_fmt() {
        set_mock(|op, special, id, addr| {
            assert_eq!(op, libc::QCMD(libc::Q_GETFMT, QuotaType::User.into()));
            assert_eq!(id, 0);
            assert_eq!(special.unwrap(), Path::new("/dev/loop999"));
            // Cast addr back to *mut i32 and write the expected format value
            let format_ptr = addr.cast::<i32>();
            unsafe {
                *format_ptr = libc::QFMT_VFS_V1; // Simulate writing VfsV1 format
            }

            Ok(())
        });
        let path = Path::new("/dev/loop999");
        let res = quota::get_fmt(QuotaType::User, path);
        assert!(res.is_ok());
        assert!(res.unwrap() == QuotaFmt::VfsV1); // Check if the format is VfsV1
        clear_mock();
    }
}

#[cfg(test)]
mod xfs_tests {
    use super::*;
    use std::path::Path;
    use xfs_quota::{
        self, FsDiskQuota, FsQuotaStateFlags, Q_XGETNEXTQUOTA, Q_XGETQSTAT, Q_XGETQSTATV,
        Q_XGETQUOTA, Q_XQUOTAOFF, Q_XQUOTAON, Q_XQUOTARM, Q_XQUOTASYNC, Q_XSETQLIM,
    };

    #[test]
    #[allow(
        clippy::cast_ptr_alignment,
        reason = "Test-only mock: addr is *mut c_char from mock syscall; actual alignment guaranteed by calling code"
    )]
    fn test_x_quota_on() {
        set_mock(|op, special, id, addr| {
            let expected_flags_val = FsQuotaStateFlags::FS_QUOTA_UDQ_ENFD.bits();
            assert_eq!(op, libc::QCMD(Q_XQUOTAON, QuotaType::User.into()));
            assert_eq!(special.unwrap(), Path::new("/mnt/xfs"));
            assert_eq!(id, 0);
            assert!(!addr.is_null());
            let flags_ptr = addr.cast::<u32>();
            let flags_val = unsafe { *flags_ptr };
            assert_eq!(flags_val, expected_flags_val);
            Ok(())
        });
        let path = Path::new("/mnt/xfs");
        let res = xfs_quota::x_quota_on(QuotaType::User, path);
        assert!(res.is_ok());
        clear_mock();
    }

    #[test]
    #[allow(
        clippy::cast_ptr_alignment,
        reason = "Test-only mock: addr is *mut c_char from mock syscall; actual alignment guaranteed by calling code"
    )]
    fn test_x_quota_off() {
        set_mock(|op, special, id, addr| {
            let expected_flags_val = FsQuotaStateFlags::FS_QUOTA_GDQ_ENFD.bits();
            assert_eq!(op, libc::QCMD(Q_XQUOTAOFF, QuotaType::Group.into()));
            assert_eq!(special.unwrap(), Path::new("/mnt/xfs"));
            assert_eq!(id, 0);
            assert!(!addr.is_null());
            let flags_ptr = addr.cast::<u32>();
            let flags_val = unsafe { *flags_ptr };
            assert_eq!(flags_val, expected_flags_val);
            Ok(())
        });
        let path = Path::new("/mnt/xfs");
        let res = xfs_quota::x_quota_off(QuotaType::Group, path);
        assert!(res.is_ok());
        clear_mock();
    }

    #[test]
    fn test_x_get_quota() {
        set_mock(|op, special, id, _addr| {
            assert_eq!(op, libc::QCMD(Q_XGETQUOTA, QuotaType::User.into()));
            assert_eq!(id, 42);
            assert_eq!(special.unwrap(), Path::new("/mnt/xfs"));
            Ok(())
        });
        let path = Path::new("/mnt/xfs");
        let res = xfs_quota::x_get_quota(QuotaType::User, path, 42);
        assert!(res.is_ok());
        clear_mock();
    }

    #[test]
    fn test_x_set_qlim() {
        set_mock(|op, special, id, _addr| {
            assert_eq!(op, libc::QCMD(Q_XSETQLIM, QuotaType::Group.into()));
            assert_eq!(id, 77);
            assert_eq!(special.unwrap(), Path::new("/mnt/xfs"));
            Ok(())
        });
        let path = Path::new("/mnt/xfs");
        let dqblk = FsDiskQuota {
            id: 77,
            ..Default::default()
        };
        let res = xfs_quota::x_set_qlim(QuotaType::Group, path, 77, dqblk);
        assert!(res.is_ok());
        clear_mock();
    }

    #[test]
    fn test_x_quota_sync() {
        set_mock(|op, special, id, addr| {
            assert_eq!(op, libc::QCMD(Q_XQUOTASYNC, QuotaType::Project.into()));
            assert!(special.is_none()); // special should not be passed
            assert_eq!(id, 0); // id should be 0
            assert!(addr.is_null()); // addr should be null
            Ok(())
        });
        let res = xfs_quota::x_quota_sync(QuotaType::Project);
        assert!(res.is_ok());
        clear_mock();
    }

    #[test]
    fn test_x_get_qstat() {
        set_mock(|op, special, id, addr| {
            assert_eq!(op, libc::QCMD(Q_XGETQSTAT, QuotaType::User.into()));
            assert_eq!(special.unwrap(), Path::new("/mnt/xfs"));
            assert_eq!(id, 0);
            assert!(!addr.is_null());
            Ok(())
        });
        let path = Path::new("/mnt/xfs");
        let res = xfs_quota::x_get_qstat(QuotaType::User, path);
        assert!(res.is_ok());
        clear_mock();
    }

    #[test]
    #[allow(
        clippy::cast_ptr_alignment,
        reason = "Test-only mock: addr is *mut c_char from mock syscall; actual alignment guaranteed by calling code"
    )]
    fn test_x_get_qstatv() {
        set_mock(|op, special, id, addr| {
            assert_eq!(op, libc::QCMD(Q_XGETQSTATV, QuotaType::Group.into()));
            assert_eq!(special.unwrap(), Path::new("/mnt/xfs"));
            assert_eq!(id, 0);
            assert!(!addr.is_null());
            let qstatv_ptr = addr.cast::<xfs_quota::fs_quota_statv>();
            assert_eq!(unsafe { (*qstatv_ptr).qs_version }, 1);
            Ok(())
        });
        let path = Path::new("/mnt/xfs");
        let res = xfs_quota::x_get_qstatv(QuotaType::Group, path);
        assert!(res.is_ok());
        clear_mock();
    }

    #[test]
    fn test_x_get_next_quota() {
        set_mock(|op, special, id, _addr| {
            assert_eq!(op, libc::QCMD(Q_XGETNEXTQUOTA, QuotaType::User.into()));
            assert_eq!(id, 42);
            assert_eq!(special.unwrap(), Path::new("/mnt/xfs"));
            Ok(())
        });
        let path = Path::new("/mnt/xfs");
        let res = xfs_quota::x_get_next_quota(QuotaType::User, path, 42);
        assert!(res.is_ok());
        clear_mock();
    }

    #[test]
    #[allow(
        clippy::cast_ptr_alignment,
        reason = "Test-only mock: addr is *mut c_char from mock syscall; actual alignment guaranteed by calling code"
    )]
    fn test_x_quota_rm() {
        set_mock(|op, special, id, addr| {
            assert_eq!(op, libc::QCMD(Q_XQUOTARM, QuotaType::Project.into()));
            assert_eq!(special.unwrap(), Path::new("/mnt/xfs"));
            assert_eq!(id, 0);
            assert!(!addr.is_null());
            let flags_ptr = addr.cast::<u16>();
            let flags_val = unsafe { *flags_ptr };
            assert_eq!(
                flags_val,
                u16::from(xfs_quota::DqFlags::FS_PROJ_QUOTA.bits())
            );
            Ok(())
        });
        let path = Path::new("/mnt/xfs");
        let res = xfs_quota::x_quota_rm(QuotaType::Project, path);
        assert!(res.is_ok());
        clear_mock();
    }
}
