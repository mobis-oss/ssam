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

/// Build the argument vector for `veritysetup format`, appending to any
/// `extra_args`. Pure: takes the already-computed `block_size`/`hash_offset`
/// so it can be tested without reading a real image's superblock/metadata.
fn build_format_args(
    block_size: u64,
    hash_offset: u64,
    include_superblock: bool,
    root_hash_file: Option<&Path>,
    image_path: &Path,
    extra_args: Vec<String>,
) -> Vec<String> {
    let block_size_str = format!("--data-block-size={block_size}");
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
    args
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
        let data_size = image_path.metadata()?.size();

        Ok(Self::from_parts(
            image_path,
            root_hash_file,
            block_size,
            data_size,
            include_superblock,
            extra_args,
        ))
    }

    fn from_parts(
        image_path: &Path,
        root_hash_file: Option<&Path>,
        block_size: u64,
        data_size: u64,
        include_superblock: bool,
        extra_args: Vec<String>,
    ) -> Self {
        // dm-verity hash area starts after data area; align data size up to block
        // boundary so hash tree begins at a valid block-aligned offset.
        let hash_offset = aligned_hash_offset(data_size, block_size);

        let args = build_format_args(
            block_size,
            hash_offset,
            include_superblock,
            root_hash_file,
            image_path,
            extra_args,
        );
        let cmd = VeritySetup {
            action: "format",
            args,
        };
        Self {
            cmd,
            image_path: image_path.to_path_buf(),
            root_hash_file: root_hash_file.map(Path::to_path_buf),
            data_size,
            hash_offset,
        }
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
    use super::{FormatVerity, VeritySetup, aligned_hash_offset, build_format_args};
    use crate::command::testing::{MockCommandRunner, MockResponse};
    use std::path::Path;
    use tempfile::tempdir;

    #[test]
    fn computes_aligned_pkgfs_verity_hash_offset() {
        assert_eq!(aligned_hash_offset(4097, 4096), 8192);
        assert_eq!(aligned_hash_offset(4096, 4096), 4096);
        assert_eq!(aligned_hash_offset(0, 4096), 0);
    }

    #[test]
    fn format_args_with_superblock_and_root_hash() {
        let args = build_format_args(
            4096,
            8192,
            true,
            Some(Path::new("/tmp/root.hash")),
            Path::new("/tmp/pkgfs.img"),
            vec![],
        );
        // include_superblock=true drops the --no-superblock flag; image path is
        // repeated (data device + hash device).
        assert_eq!(
            args,
            vec![
                "--data-block-size=4096",
                "--hash-offset=8192",
                "--root-hash-file=/tmp/root.hash",
                "/tmp/pkgfs.img",
                "/tmp/pkgfs.img",
            ]
        );
    }

    #[test]
    fn format_args_without_superblock_or_root_hash() {
        let args = build_format_args(512, 1024, false, None, Path::new("img"), vec![]);
        assert_eq!(
            args,
            vec![
                "--data-block-size=512",
                "--hash-offset=1024",
                "--no-superblock",
                "img",
                "img",
            ]
        );
    }

    #[test]
    fn format_args_prepends_extra_args() {
        let args = build_format_args(
            4096,
            4096,
            true,
            None,
            Path::new("img"),
            vec!["--foo".to_owned()],
        );
        assert_eq!(args[0], "--foo");
        assert_eq!(args[1], "--data-block-size=4096");
    }

    // --- FormatVerity::from_parts (A) ---

    #[test]
    fn from_parts_builds_format_command_and_offset() {
        let image = Path::new("/tmp/pkgfs.img");
        let root_hash = Path::new("/tmp/root.hash");
        let fv = FormatVerity::from_parts(image, Some(root_hash), 4096, 8000, true, vec![]);

        // hash_offset aligns data_size up to the block boundary.
        assert_eq!(fv.hash_offset, aligned_hash_offset(8000, 4096));
        assert_eq!(fv.hash_offset, 8192);
        assert_eq!(fv.data_size, 8000);
        assert_eq!(fv.root_hash_file.as_deref(), Some(root_hash));
        assert_eq!(fv.image_path, image);
        assert_eq!(fv.cmd.action, "format");
        assert_eq!(
            fv.cmd.args,
            build_format_args(4096, 8192, true, Some(root_hash), image, vec![])
        );
    }

    // --- VeritySetup::run command wiring (B) ---

    #[test]
    fn veritysetup_run_prepends_action_and_captures_both_streams() {
        let vs = VeritySetup {
            action: "format",
            args: vec!["--foo".to_owned(), "img".to_owned()],
        };
        let mock = MockCommandRunner::new();
        let handle = mock.clone();

        vs.run(&mock).unwrap();

        let calls = handle.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].cmd, "veritysetup");
        assert_eq!(calls[0].arg_strs(), vec!["format", "--foo", "img"]);
        // veritysetup output is captured so FormatVerity::run can inspect it.
        assert!(calls[0].capture.stdout);
        assert!(calls[0].capture.stderr);
    }

    // --- FormatVerity::run error branches (C) ---

    #[test]
    fn run_bails_on_command_failure() {
        let dir = tempdir().unwrap();
        let img = dir.path().join("pkgfs.img");
        std::fs::write(&img, vec![0u8; 100]).unwrap();
        let fv = FormatVerity::from_parts(&img, None, 4096, 100, true, vec![]);

        let mock = MockCommandRunner::with_responses(vec![MockResponse::failure(1, "boom")]);
        let err = fv.run(&mock).unwrap_err();
        assert!(
            err.to_string()
                .contains("Failed to create dm-verity hash tree")
        );
    }

    #[test]
    fn run_bails_when_hash_offset_exceeds_file_size() {
        let dir = tempdir().unwrap();
        let img = dir.path().join("pkgfs.img");
        std::fs::write(&img, vec![0u8; 100]).unwrap(); // 100-byte file

        // data_size=100 -> hash_offset = aligned(100, 4096) = 4096 > 100.
        let fv = FormatVerity::from_parts(&img, None, 4096, 100, true, vec![]);

        // Command "succeeds" but the mock doesn't grow the file, so the hash
        // area would start past EOF.
        let mock = MockCommandRunner::new();
        let err = fv.run(&mock).unwrap_err();
        assert!(err.to_string().contains("Image file smaller than expected"));
    }
}
