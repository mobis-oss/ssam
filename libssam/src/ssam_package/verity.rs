// Copyright 2026 Hyundai Mobis Co., Ltd.
// SPDX-License-Identifier: Apache-2.0

use std::{
    fmt::Write,
    fs::File,
    io::{Seek, SeekFrom},
    path::Path,
};

use anyhow::Context;

use super::PackageFsVerityInfo;
use crate::config::SSAM_SERIALIZATION_CONFIG;

#[repr(C, packed)]
#[derive(Debug, bincode::Decode)]
struct VeritySuperBlock {
    _signature: [u8; 8],
    version: u32,
    _hash_type: u32,
    _uuid: [u8; 16],
    algorithm: [u8; 32],
    data_block_size: u32,
    hash_block_size: u32,
    data_blocks: u64,
    salt_size: u16,
    _pad1: [u8; 6],
    salt: [u8; 256],
    _pad2: [u8; 168],
}

impl VeritySuperBlock {
    fn get_algorithm(&self) -> anyhow::Result<String> {
        let algorithm =
            String::from_utf8(self.algorithm.into_iter().take_while(|c| *c != 0).collect())?;
        // The algorithm is spliced verbatim into the space-separated dm-verity
        // table, so whitespace would shift every following argument. Reject it
        // to keep the signed table's field layout unforgeable.
        anyhow::ensure!(
            !algorithm.contains(char::is_whitespace),
            "Invalid dm-verity superblock: algorithm field contains whitespace"
        );
        Ok(algorithm)
    }

    fn get_salt(&self) -> anyhow::Result<String> {
        let salt_size = usize::from(self.salt_size);
        anyhow::ensure!(
            salt_size <= self.salt.len(),
            "Invalid dm-verity superblock: salt_size ({salt_size}) exceeds salt buffer length ({})",
            self.salt.len()
        );
        if salt_size == 0 {
            // The kernel dm-verity table uses "-" as the no-salt sentinel; an
            // empty field would drop an argument and fail the table load.
            return Ok("-".to_string());
        }
        let salt_slice: &[u8] = &self.salt[..salt_size];
        let salt_hex = salt_slice.iter().fold(String::new(), |mut acc, byte| {
            write!(acc, "{byte:02x}").unwrap();
            acc
        });
        Ok(salt_hex)
    }
}

fn read_super_block(hash_file: &mut File, hash_offset: u64) -> anyhow::Result<VeritySuperBlock> {
    hash_file.seek(SeekFrom::Start(hash_offset))?;
    Ok(bincode::decode_from_std_read(
        hash_file,
        SSAM_SERIALIZATION_CONFIG,
    )?)
}

impl PackageFsVerityInfo {
    /// Build a signed [`PackageFsVerityInfo`] by reading the dm-verity superblock
    /// from a freshly formatted package filesystem image.
    ///
    /// The superblock is untrusted at runtime, so its algorithm/salt/block-size
    /// fields are captured here (packaging time, before signing) and baked into
    /// [`PackageFsVerityInfo::table_params`]. Once signed, `ssamd` never parses
    /// the superblock again — closing the argument-injection hole.
    ///
    /// # Errors
    ///
    /// Returns an error if the image cannot be opened, the superblock cannot be
    /// decoded, or the superblock reports zero block sizes.
    pub fn from_verity_image(
        image_path: &Path,
        data_size: u64,
        hash_size: u64,
        hash_offset: u64,
        root_hash: &str,
    ) -> anyhow::Result<Self> {
        let mut hash_file = File::open(image_path).with_context(|| {
            format!(
                "Failed to open verity image for superblock read: {}",
                image_path.display()
            )
        })?;
        let sb = read_super_block(&mut hash_file, hash_offset)
            .context("Failed to read dm-verity superblock while building signed table")?;

        let version = sb.version;
        let data_block_size = sb.data_block_size;
        let hash_block_size = sb.hash_block_size;
        let data_blocks = sb.data_blocks;
        let algorithm = sb.get_algorithm()?;
        let salt = sb.get_salt()?;

        anyhow::ensure!(
            data_block_size != 0 && hash_block_size != 0,
            "Invalid dm-verity superblock: block sizes must be non-zero \
             (data_block_size={data_block_size}, hash_block_size={hash_block_size})"
        );

        let hash_start_block = data_size / u64::from(data_block_size) + 1;

        let table_params = Self::build_table_params(
            version,
            data_block_size,
            hash_block_size,
            data_blocks,
            hash_start_block,
            &algorithm,
            root_hash,
            &salt,
        );

        Ok(Self {
            data_size,
            hash_size,
            table_params,
            hash_offset,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as IoWrite;
    use tempfile::{NamedTempFile, TempDir};

    fn make_super_block(algorithm: &str, salt_data: &[u8]) -> VeritySuperBlock {
        let mut sb = VeritySuperBlock {
            _signature: *b"verity\0\0",
            version: 1,
            _hash_type: 1,
            _uuid: [0; 16],
            algorithm: [0; 32],
            data_block_size: 4096,
            hash_block_size: 4096,
            data_blocks: 100,
            salt_size: 0,
            _pad1: [0; 6],
            salt: [0; 256],
            _pad2: [0; 168],
        };
        sb.algorithm[..algorithm.len()].copy_from_slice(algorithm.as_bytes());
        sb.salt_size = u16::try_from(salt_data.len()).expect("salt fits u16");
        sb.salt[..salt_data.len()].copy_from_slice(salt_data);
        sb
    }

    fn write_super_block(
        temp_dir: &TempDir,
        sb: VeritySuperBlock,
    ) -> anyhow::Result<NamedTempFile> {
        let mut hash_file = NamedTempFile::new_in(temp_dir)?;
        let sb_bytes: [u8; 512] = unsafe { std::mem::transmute(sb) };
        hash_file.write_all(&sb_bytes)?;
        hash_file.flush()?;
        Ok(hash_file)
    }

    #[test]
    fn get_algorithm_reads_nul_terminated_string() -> anyhow::Result<()> {
        let sb = make_super_block("sha256", b"");
        assert_eq!(sb.get_algorithm()?, "sha256");
        Ok(())
    }

    #[test]
    fn get_algorithm_rejects_invalid_utf8() {
        let mut sb = make_super_block("sha256", b"");
        sb.algorithm = [0xFF; 32];
        assert!(sb.get_algorithm().is_err());
    }

    #[test]
    fn get_algorithm_rejects_whitespace() {
        let sb = make_super_block("sha256 injected", b"");
        assert!(sb.get_algorithm().is_err());
    }

    #[test]
    fn get_salt_hex_encodes_bytes() -> anyhow::Result<()> {
        let sb = make_super_block("sha256", &[0x00, 0x01, 0x02, 0x03]);
        assert_eq!(sb.get_salt()?, "00010203");
        Ok(())
    }

    #[test]
    fn get_salt_returns_dash_when_empty() -> anyhow::Result<()> {
        let sb = make_super_block("sha256", b"");
        assert_eq!(sb.get_salt()?, "-");
        Ok(())
    }

    #[test]
    fn get_salt_rejects_oversized_salt_size() {
        let mut sb = make_super_block("sha256", b"");
        sb.salt_size = 257;
        assert!(sb.get_salt().is_err());
    }

    #[test]
    fn read_super_block_fails_on_empty_file() -> anyhow::Result<()> {
        let temp_dir = TempDir::new()?;
        let mut empty = NamedTempFile::new_in(&temp_dir)?;
        assert!(read_super_block(empty.as_file_mut(), 0).is_err());
        Ok(())
    }

    #[test]
    fn from_verity_image_bakes_superblock_fields_into_table() -> anyhow::Result<()> {
        let temp_dir = TempDir::new()?;
        let salt = [0xAB, 0xCD, 0xEF];
        let hash_file = write_super_block(&temp_dir, make_super_block("sha256", &salt))?;

        let info =
            PackageFsVerityInfo::from_verity_image(hash_file.path(), 8192, 512, 0, "cafebabe")?;

        // data_size(8192) / data_block_size(4096) + 1 = 3
        assert_eq!(
            info.table_params,
            "1 4096 4096 100 3 sha256 cafebabe abcdef"
        );
        assert_eq!(
            info.resolve_table("/dev/loop0")?,
            "1 /dev/loop0 /dev/loop0 4096 4096 100 3 sha256 cafebabe abcdef"
        );
        Ok(())
    }
}
