// Copyright 2025-2026 Hyundai Mobis Co., Ltd.
// SPDX-License-Identifier: Apache-2.0

use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::{io::Seek, path::Path};
use strum::Display;

use crate::config::SSAM_SERIALIZATION_CONFIG;

pub(crate) trait SuperBlockReader: Sized + SuperBlock + bincode::Decode<Self>
where
    Self: 'static,
{
    fn get_superblock(path: impl AsRef<Path>) -> anyhow::Result<Box<dyn SuperBlock>>
    where
        Self: bincode::Decode<()>,
    {
        let path = path.as_ref();
        let mut fd = std::fs::File::open(path).context("File open error")?;
        fd.seek(std::io::SeekFrom::Start(1024))?;

        let sb: Self = bincode::decode_from_std_read(&mut fd, SSAM_SERIALIZATION_CONFIG)
            .with_context(|| {
                format!(
                    "Cannot get correct superblock. Please check the file: {}",
                    path.display()
                )
            })?;
        if !sb.verify() {
            anyhow::bail!("Invalid filesystem, check if the file is a correct EROFS image.");
        }
        Ok(Box::new(sb))
    }

    fn verify(&self) -> bool;
}

#[derive(
    Debug, Clone, Copy, Display, PartialEq, bincode::Encode, bincode::Decode, Serialize, Deserialize,
)]
#[strum(serialize_all = "lowercase")]
#[cfg_attr(test, derive(Default))]
pub enum FsType {
    Erofs,
    #[cfg_attr(test, default)]
    Ext4,
}

pub trait SuperBlock {
    /// Returns the filesystem block size in bytes.
    ///
    /// # Errors
    ///
    /// Returns an error if the on-disk block size field is out of the valid range.
    fn block_size(&self) -> anyhow::Result<u32>;
    fn fs_type(&self) -> FsType;
}

pub struct FileSystemSuperBlockBroker {
    superblock: Box<dyn SuperBlock>,
}

impl std::ops::Deref for FileSystemSuperBlockBroker {
    type Target = dyn SuperBlock;

    fn deref(&self) -> &Self::Target {
        &*self.superblock
    }
}

mod erofs {
    use super::FsType;
    use super::{SuperBlock, SuperBlockReader};
    use serde::Deserialize;

    #[derive(Debug, Deserialize, bincode::Decode, bytemuck::Zeroable)]
    #[repr(C)]
    pub(crate) struct ErofsSuperBlock {
        magic: u32,
        checksum: i32,
        feature_compat: i32,
        blkszbits: u8,
        sb_extslots: u8,
        root_nid: i16,
        inos: i64,
        build_time: i64,
        build_time_nsec: i32,
        blocks: i32,
        meta_blkaddr: u32,
        xattr_blkaddr: u32,
        uuid: [u8; 16],
        volume_name: [u8; 16],
        feature_incompat: i32,
        compression: i16,
        extra_devices: i16,
        devt_slotoff: i16,
        dirblkbits: u8,
        xattr_prefix_count: u8,
        xattr_prefix_start: i32,
        packed_nid: i64,
        xattr_filter_reserved: u8,
        reserved: [u8; 23],
    }

    const EROFS_SUPER_MAGIC: u32 = 0xE0F5_E1E2;

    // Kernel validates blkszbits <= PAGE_SHIFT (12 on x86_64, up to 16 on ARM64
    // with 64 KiB pages). We accept the maximum across all supported targets
    // since ssam-wrap cross-builds packages. Values >= 32 overflow `2u32.pow()`.
    const EROFS_MAX_BLKSZBITS: u8 = 16;

    impl SuperBlock for ErofsSuperBlock {
        fn block_size(&self) -> anyhow::Result<u32> {
            anyhow::ensure!(
                self.blkszbits <= EROFS_MAX_BLKSZBITS,
                "EROFS blkszbits {} exceeds maximum {}",
                self.blkszbits,
                EROFS_MAX_BLKSZBITS,
            );
            Ok(2u32.pow(u32::from(self.blkszbits)))
        }

        fn fs_type(&self) -> FsType {
            FsType::Erofs
        }
    }

    impl SuperBlockReader for ErofsSuperBlock {
        fn verify(&self) -> bool {
            self.magic == EROFS_SUPER_MAGIC
        }
    }

    #[cfg(test)]
    mod tests {
        use bytemuck::Zeroable;

        use super::*;

        fn make_sb(magic: u32, blkszbits: u8) -> ErofsSuperBlock {
            let mut sb = ErofsSuperBlock::zeroed();
            sb.magic = magic;
            sb.blkszbits = blkszbits;
            sb
        }

        #[test]
        fn verify_accepts_valid_magic() {
            assert!(make_sb(EROFS_SUPER_MAGIC, 12).verify());
        }

        #[test]
        fn verify_rejects_bad_magic() {
            assert!(!make_sb(0xDEAD_BEEF, 12).verify());
        }

        #[test]
        fn block_size_valid() {
            assert_eq!(make_sb(EROFS_SUPER_MAGIC, 12).block_size().unwrap(), 4096);
            assert_eq!(
                make_sb(EROFS_SUPER_MAGIC, EROFS_MAX_BLKSZBITS)
                    .block_size()
                    .unwrap(),
                65536
            );
        }

        #[test]
        fn block_size_rejects_overflow() {
            // 2u32.pow(32) overflows u32
            const EROFS_OVERFLOW_BLKSZBITS: u8 = 32;

            assert!(
                make_sb(EROFS_SUPER_MAGIC, EROFS_MAX_BLKSZBITS + 1)
                    .block_size()
                    .is_err()
            );
            assert!(
                make_sb(EROFS_SUPER_MAGIC, EROFS_OVERFLOW_BLKSZBITS)
                    .block_size()
                    .is_err()
            );
            assert!(make_sb(EROFS_SUPER_MAGIC, u8::MAX).block_size().is_err());
        }
    }
}

mod ext4 {
    use super::{FsType, SuperBlock, SuperBlockReader};
    use serde::Deserialize;
    #[derive(Debug, Deserialize, bincode::Decode, bytemuck::Zeroable)]
    #[repr(C)]
    // Field names mirror the Linux ext4 superblock struct (s_ prefix is part of the kernel ABI).
    #[allow(clippy::struct_field_names)]
    pub(crate) struct Ext4SuperBlock {
        s_inodes_count: u32,
        s_blocks_count_lo: u32,
        s_r_blocks_count_lo: u32,
        s_free_blocks_count_lo: u32,
        s_free_inodes_count: u32,
        s_first_data_block: u32,
        s_log_block_size: u32,
        s_log_cluster_size: u32,
        s_blocks_per_group: u32,
        s_clusters_per_group: u32,
        s_inodes_per_group: u32,
        s_mtime: u32,
        s_wtime: u32,
        s_mnt_count: u16,
        s_max_mnt_count: u16,
        s_magic: u16,
        s_state: u16,
        s_errors: u16,
        s_minor_rev_level: u16,
        s_lastcheck: u32,
        s_checkinterval: u32,
        s_creator_os: u32,
        s_rev_level: u32,
        s_def_resuid: u16,
        s_def_resgid: u16,
        s_first_ino: u32,
        s_inode_size: u16,
        s_block_group_nr: u16,
        s_feature_compat: u32,
        s_feature_incompat: u32,
        s_feature_ro_compat: u32,
        s_uuid: [u8; 16],
        s_volume_name: [u8; 16],
        #[serde(with = "serde_big_array::BigArray")]
        s_last_mounted: [u8; 64],
        s_algorithm_usage_bitmap: u32,
        s_prealloc_blocks: u8,
        s_prealloc_dir_blocks: u8,
        s_reserved_gdt_blocks: u16,
        s_journal_uuid: [u8; 16],
        s_journal_inum: u32,
        s_journal_dev: u32,
        s_last_orphan: u32,
        s_hash_seed: [u32; 4],
        s_def_hash_version: u8,
        s_jnl_backup_type: u8,
        s_desc_size: u16,
        s_default_mount_opts: u32,
        s_first_meta_bg: u32,
        s_mkfs_time: u32,
        s_jnl_blocks: [u32; 17],
        s_blocks_count_hi: u32,
        s_r_blocks_count_hi: u32,
        s_free_blocks_count_hi: u32,
        s_min_extra_isize: u16,
        s_want_extra_isize: u16,
        s_flags: u32,
        s_raid_stride: u16,
        s_mmp_update_interval: u16,
        s_mmp_block: u64,
        s_raid_stripe_width: u32,
        s_log_groups_per_flex: u8,
        s_checksum_type: u8,
        s_encryption_level: u8,
        s_reserved_pad: u8,
        s_kbytes_written: u64,
        s_snapshot_inum: u32,
        s_snapshot_id: u32,
        s_snapshot_r_blocks_count: u64,
        s_snapshot_list: u32,
        s_error_count: u32,
        s_first_error_time: u32,
        s_first_error_ino: u32,
        s_first_error_block: u64,
        s_first_error_func: [u8; 32],
        s_first_error_line: u32,
        s_last_error_time: u32,
        s_last_error_ino: u32,
        s_last_error_line: u32,
        s_last_error_block: u64,
        s_last_error_func: [u8; 32],
        #[serde(with = "serde_big_array::BigArray")]
        s_mount_opts: [u8; 64],
        s_usr_quota_inum: u32,
        s_grp_quota_inum: u32,
        s_overhead_clusters: u32,
        s_backup_bgs: [u32; 2],
        s_encrypt_algos: [u8; 4],
        s_encrypt_pw_salt: [u8; 16],
        s_lpf_ino: u32,
        s_prj_quota_inum: u32,
        s_checksum_seed: u32,
        s_wtime_hi: u8,
        s_mtime_hi: u8,
        s_mkfs_time_hi: u8,
        s_lastcheck_hi: u8,
        s_first_error_time_hi: u8,
        s_last_error_time_hi: u8,
        s_pad: [u8; 2],
        s_encoding: u16,
        s_encoding_flags: u16,
        #[serde(with = "serde_big_array::BigArray")]
        s_reserved: [u32; 95],
        s_checksum: u32,
    }

    const EXT4_SUPER_MAGIC: u16 = 0xEF53;

    // Kernel (fs/ext4/super.c) rejects s_log_block_size > EXT4_MAX_BLOCK_LOG_SIZE (6),
    // which corresponds to a maximum block size of 1024 << 6 = 65536 bytes (64 KiB).
    // Values >= 22 would overflow u32 (1024 << 22 = 2^32).
    const EXT4_MAX_S_LOG_BLOCK_SIZE: u32 = 6;

    impl SuperBlock for Ext4SuperBlock {
        fn block_size(&self) -> anyhow::Result<u32> {
            anyhow::ensure!(
                self.s_log_block_size <= EXT4_MAX_S_LOG_BLOCK_SIZE,
                "ext4 s_log_block_size {} exceeds maximum {}",
                self.s_log_block_size,
                EXT4_MAX_S_LOG_BLOCK_SIZE,
            );
            Ok(1024u32 << self.s_log_block_size)
        }

        fn fs_type(&self) -> FsType {
            FsType::Ext4
        }
    }

    impl SuperBlockReader for Ext4SuperBlock {
        fn verify(&self) -> bool {
            self.s_magic == EXT4_SUPER_MAGIC
        }
    }

    #[cfg(test)]
    mod tests {
        use bytemuck::Zeroable;

        use super::*;

        fn make_sb(magic: u16, log_block_size: u32) -> Ext4SuperBlock {
            let mut sb = Ext4SuperBlock::zeroed();
            sb.s_magic = magic;
            sb.s_log_block_size = log_block_size;
            sb
        }

        #[test]
        fn verify_accepts_valid_magic() {
            assert!(make_sb(EXT4_SUPER_MAGIC, 0).verify());
        }

        #[test]
        fn verify_rejects_bad_magic() {
            assert!(!make_sb(0x0000, 0).verify());
        }

        #[test]
        fn block_size_valid() {
            assert_eq!(make_sb(EXT4_SUPER_MAGIC, 0).block_size().unwrap(), 1024);
            assert_eq!(
                make_sb(EXT4_SUPER_MAGIC, EXT4_MAX_S_LOG_BLOCK_SIZE)
                    .block_size()
                    .unwrap(),
                65536
            );
        }

        #[test]
        fn block_size_rejects_overflow() {
            // 1024 << 22 == 2^32: u32 overflow point
            const EXT4_OVERFLOW_LOG_BLOCK_SIZE: u32 = 22;

            assert!(
                make_sb(EXT4_SUPER_MAGIC, EXT4_MAX_S_LOG_BLOCK_SIZE + 1)
                    .block_size()
                    .is_err()
            );
            assert!(
                make_sb(EXT4_SUPER_MAGIC, EXT4_OVERFLOW_LOG_BLOCK_SIZE)
                    .block_size()
                    .is_err()
            );
            assert!(make_sb(EXT4_SUPER_MAGIC, u32::MAX).block_size().is_err());
        }
    }
}

impl FileSystemSuperBlockBroker {
    /// Opens the file at `path` and attempts to parse it as an erofs or ext4 superblock.
    ///
    /// # Errors
    ///
    /// Returns an error if the file at `path` cannot be read as a valid erofs or ext4 superblock.
    pub fn new(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let path = path.as_ref();

        let sb = erofs::ErofsSuperBlock::get_superblock(path)
            .or_else(|_| ext4::Ext4SuperBlock::get_superblock(path))
            .with_context(|| format!("Error while getting filesystem superblock from {}. The file might not be the type of erofs or ext4", path.display())
            )?;
        Ok(Self { superblock: sb })
    }
}
