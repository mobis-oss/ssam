// Copyright 2025-2026 Hyundai Mobis Co., Ltd.
// SPDX-License-Identifier: Apache-2.0

use anyhow::Context;
use clap::ValueEnum;
use std::ffi::OsStr;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use strum::{Display, EnumString, VariantNames};

mod docker;
mod image;

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, ValueEnum, EnumString, Display, VariantNames, Default,
)]
#[strum(serialize_all = "kebab-case")]
pub enum ImageType {
    Ext4,
    Erofs,
    #[strum(serialize = "erofs-lz4")]
    ErofsLz4,
    #[strum(serialize = "erofs-lz4hc")]
    #[default]
    ErofsLz4hc,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Display, Default)]
pub enum OciArchitecture {
    #[strum(serialize = "arm64")]
    #[default]
    Arm64,
    #[strum(serialize = "amd64")]
    Amd64,
}

/* Part of Skopeo available transports are supported.
Refer to the following for detail:
- https://github.com/containers/image/blob/main/docs/containers-transports.5.md
- https://man.archlinux.org/man/skopeo.1.en#IMAGE_NAMES */
pub const SUPPORTED_CONTAINER_TRANSPORTS: &[&str] = &[
    "docker://",
    "docker-daemon:",
    "docker-archive:",
    "oci-archive:",
    "oci:",
];

impl From<ImageType> for (&'static str, Vec<&'static str>) {
    fn from(image_type: ImageType) -> Self {
        match image_type {
            ImageType::Ext4 => ("pkgfs.ext4", vec!["mkfs.ext4"]),
            ImageType::Erofs => ("pkgfs.erofs", vec!["mkfs.erofs"]),
            ImageType::ErofsLz4 => ("pkgfs.erofs-lz4", vec!["mkfs.erofs", "-zlz4"]),
            ImageType::ErofsLz4hc => ("pkgfs.erofs-lz4hc", vec!["mkfs.erofs", "-zlz4hc"]),
        }
    }
}

pub fn execute_command<I, S>(
    cmd: &str,
    args: Option<I>,
    capture_stdout: bool,
) -> anyhow::Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut command = Command::new(cmd);
    if let Some(args) = args {
        command.args(args);
    }

    if capture_stdout {
        command.stdout(Stdio::piped());
    } else {
        command.stdout(Stdio::inherit());
    }
    command.stderr(Stdio::inherit());

    let output = command.output().context(format!("Failed to run {cmd}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!(
            "Command {:?} failed with status {}\nStderr: {}",
            command,
            output.status,
            stderr
        );
    }

    if !capture_stdout || output.stdout.is_empty() {
        Ok(String::new())
    } else {
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        Ok(stdout)
    }
}

// Safety: Using Debug format ({:#?}/{:?}) for PathBuf instead of Display to prevent
// log injection attacks via special characters in path names.
#[allow(clippy::use_debug, clippy::unnecessary_debug_formatting)]
pub fn prepare(
    workspace: &crate::Workspace,
    pkgfs_src: Option<&str>,
    oci_arch: Option<OciArchitecture>,
) -> anyhow::Result<()> {
    if let Some(pkgfs_src) = pkgfs_src {
        // using symlink_metadata() because pkgfs may be a symlink
        if workspace.pkgfs.symlink_metadata().is_ok() {
            println!("Warn: pkgfs already exists. It will be overwritten.");
            fs::remove_dir_all(&workspace.pkgfs)
                .with_context(|| format!("Failed to remove pkgfs: {:#?}", workspace.pkgfs))?;
        }

        if SUPPORTED_CONTAINER_TRANSPORTS
            .iter()
            .any(|prefix| pkgfs_src.starts_with(prefix))
        {
            // If src is docker, creating pkgfs as a directory with the extracted rootfs
            docker::extract_rootfs(workspace, pkgfs_src, oci_arch.unwrap_or_default())?;
        } else {
            if oci_arch.is_some() {
                println!("WARN: Given source OCI architecture would be ignored!");
            }
            let pkgfs_src = PathBuf::from(pkgfs_src).canonicalize().with_context(|| {
                format!("Failed to determine package filesystem source - {pkgfs_src:#?}")
            })?;

            std::os::unix::fs::symlink(&pkgfs_src, &workspace.pkgfs).with_context(|| {
                format!(
                    "Failed to create symlink for package filesystem source: {:#?} -> {:#?}",
                    pkgfs_src, workspace.pkgfs
                )
            })?;
        }
    } else if !workspace.pkgfs.exists() {
        // If src is not provided, use pkgfs as source since it will be filled later by the user
        fs::create_dir_all(&workspace.pkgfs).context(format!(
            "Failed to create pkgfs directory: {:#?}",
            workspace.pkgfs
        ))?;
    }

    Ok(())
}

fn get_pkgfs_src(workspace: &crate::Workspace) -> anyhow::Result<PathBuf> {
    let pkgfs = &workspace.pkgfs;
    if !pkgfs.exists() {
        anyhow::bail!(r#"pkgfs - "{}" does not exist"#, pkgfs.display())
    } else if pkgfs.is_symlink() {
        pkgfs
            .canonicalize()
            .context(r#"Location where pkgfs - "{}" points is wrong or does not exist"#)
    } else if pkgfs.is_dir() {
        Ok(pkgfs.clone())
    } else {
        anyhow::bail!(r#"pkgfs - "{}" is not valid!"#, pkgfs.display())
    }
}

pub fn build_image(
    workspace: &crate::Workspace,
    pkgfs_type: Option<ImageType>,
) -> anyhow::Result<PathBuf> {
    let pkgfs_src = get_pkgfs_src(workspace)?;

    if pkgfs_src.is_file() {
        println!("INFO: Package filesystem source is a file. Will treat it as an image file.");
        if pkgfs_type.is_some() {
            println!("WARN: Given package filesystem type would be ignored!");
        }
        Ok(pkgfs_src)
    } else if pkgfs_src.is_dir() {
        let pkgfs_type = pkgfs_type.unwrap_or_default();
        println!("Creating package filesystem image as {pkgfs_type}");
        image::create(workspace, pkgfs_src, pkgfs_type)
    } else {
        anyhow::bail!(
            "Package filesystem source is neither a file nor a directory: {}",
            pkgfs_src.display()
        );
    }
}
