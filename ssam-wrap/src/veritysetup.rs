// Copyright 2025-2026 Hyundai Mobis Co., Ltd.
// SPDX-License-Identifier: Apache-2.0

use std::{
    ffi::OsStr,
    fs,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    process::Output,
};

use anyhow::Context;
use libssam::{ssam_package::PackageFsVerityInfo, superblock};

use crate::command::{CommandRunner, OutputCapture};

/// Returns the byte offset at which the dm-verity hash tree starts inside a
/// combined `filesystem.img`, rounded up to the next `block_size` boundary.
pub fn aligned_hash_offset(data_size: u64, block_size: u64) -> u64 {
    data_size.div_ceil(block_size) * block_size
}

struct VeritySetup<'vs> {
    action: &'vs str,
    args: Vec<String>,
}

impl VeritySetup<'_> {
    const VERITY_SETUP_CMD: &'static str = "veritysetup";
    pub fn run(&self, runner: &dyn CommandRunner) -> anyhow::Result<Output> {
        let mut all_args: Vec<&OsStr> = Vec::with_capacity(self.args.len() + 1);
        all_args.push(OsStr::new(self.action));
        all_args.extend(self.args.iter().map(|s| OsStr::new(s.as_str())));
        runner
            .run(
                VeritySetup::VERITY_SETUP_CMD,
                &all_args,
                OutputCapture {
                    stdout: true,
                    stderr: true,
                },
            )
            .context(format!(
                "Failed to run {} {} {}",
                VeritySetup::VERITY_SETUP_CMD,
                self.action,
                self.args.join(" ")
            ))
    }
}

pub struct FormatVerity<'action> {
    cmd: VeritySetup<'action>,
    image_path: PathBuf,
    root_hash_file: Option<PathBuf>,
    data_size: u64,
    hash_offset: u64,
}

impl FormatVerity<'_> {
    pub fn new(
        image_path: &Path,
        root_hash_file: Option<&Path>,
        include_superblock: bool,
        extra_args: Vec<String>,
    ) -> anyhow::Result<Self> {
        let fs_superblock = superblock::FileSystemSuperBlockBroker::new(image_path)?;

        let block_size = u64::from(fs_superblock.block_size()?);
        let block_size_str = format!("--data-block-size={block_size}");
        // dm-verity hash area starts after data area; align data size up to block
        // boundary so hash tree begins at a valid block-aligned offset.
        let data_size = image_path.metadata()?.size();
        let hash_offset = aligned_hash_offset(data_size, block_size);
        let hash_offset_str = format!("--hash-offset={hash_offset}");

        let superblock = if include_superblock {
            String::default()
        } else {
            "--no-superblock".to_owned()
        };
        let root_hash_file_str = root_hash_file
            .as_ref()
            .map(|p| format!("--root-hash-file={}", p.display()))
            .unwrap_or_default();
        let image_path_str = image_path.display().to_string();

        let mut args = extra_args;
        args.extend(
            [
                block_size_str,
                hash_offset_str,
                superblock,
                root_hash_file_str,
                image_path_str.clone(),
                image_path_str,
            ]
            .into_iter()
            .filter(|s| !s.is_empty()),
        );
        let cmd = VeritySetup {
            action: "format",
            args,
        };
        Ok(Self {
            cmd,
            image_path: image_path.to_path_buf(),
            root_hash_file: root_hash_file.map(Path::to_path_buf),
            data_size,
            hash_offset,
        })
    }

    pub fn run(&self, runner: &dyn CommandRunner) -> anyhow::Result<PackageFsVerityInfo> {
        let output = self.cmd.run(runner)?;
        if !output.status.success() {
            anyhow::bail!(
                "Failed to create dm-verity hash tree: process exited with status {}. Errmsg: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            );
        }
        let image_size_after = self
            .image_path
            .metadata()
            .context("Failed to get pkgfs image file metadata after verity formatting")?
            .len();
        let hash_size = image_size_after.checked_sub(self.hash_offset).context(
            "Image file smaller than expected after verity formatting: hash_offset exceeds file size",
        )?;
        let root_hash = match &self.root_hash_file {
            Some(p) => fs::read_to_string(p)
                .context("Failed to read root hash file")?
                .trim()
                .to_string(),
            None => String::new(),
        };
        PackageFsVerityInfo::from_verity_image(
            &self.image_path,
            self.data_size,
            hash_size,
            self.hash_offset,
            &root_hash,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::aligned_hash_offset;

    #[test]
    fn computes_aligned_pkgfs_verity_hash_offset() {
        assert_eq!(aligned_hash_offset(4097, 4096), 8192);
        assert_eq!(aligned_hash_offset(4096, 4096), 4096);
        assert_eq!(aligned_hash_offset(0, 4096), 0);
    }
}
