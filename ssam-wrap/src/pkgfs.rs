// Copyright 2025-2026 Hyundai Mobis Co., Ltd.
// SPDX-License-Identifier: Apache-2.0

use anyhow::Context;
use clap::ValueEnum;
use std::fs;
use std::path::PathBuf;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::testing::MockCommandRunner;
    use std::os::unix::fs::symlink;
    use std::path::Path;
    use tempfile::tempdir;

    fn workspace_at(root: &Path) -> crate::Workspace {
        crate::Workspace::new(root, Box::new(MockCommandRunner::new()))
    }

    #[test]
    fn image_type_maps_to_filename_and_mkfs_args() {
        let cases = [
            (ImageType::Ext4, "pkgfs.ext4", vec!["mkfs.ext4"]),
            (ImageType::Erofs, "pkgfs.erofs", vec!["mkfs.erofs"]),
            (
                ImageType::ErofsLz4,
                "pkgfs.erofs-lz4",
                vec!["mkfs.erofs", "-zlz4"],
            ),
            (
                ImageType::ErofsLz4hc,
                "pkgfs.erofs-lz4hc",
                vec!["mkfs.erofs", "-zlz4hc"],
            ),
        ];
        for (ty, name, args) in cases {
            let (n, a): (&str, Vec<&str>) = ty.into();
            assert_eq!(n, name);
            assert_eq!(a, args);
        }
    }

    #[test]
    fn image_type_strum_roundtrip_and_default() {
        assert_eq!(ImageType::default(), ImageType::ErofsLz4hc);
        assert_eq!(ImageType::ErofsLz4hc.to_string(), "erofs-lz4hc");
        assert_eq!(ImageType::ErofsLz4.to_string(), "erofs-lz4");
        assert_eq!("ext4".parse::<ImageType>().unwrap(), ImageType::Ext4);
        assert_eq!(
            "erofs-lz4hc".parse::<ImageType>().unwrap(),
            ImageType::ErofsLz4hc
        );
        assert!("bogus".parse::<ImageType>().is_err());
    }

    #[test]
    fn oci_architecture_display_and_default() {
        assert_eq!(OciArchitecture::default(), OciArchitecture::Arm64);
        assert_eq!(OciArchitecture::Arm64.to_string(), "arm64");
        assert_eq!(OciArchitecture::Amd64.to_string(), "amd64");
    }

    #[test]
    fn supported_transports_prefix_matching() {
        let is_container = |s: &str| {
            SUPPORTED_CONTAINER_TRANSPORTS
                .iter()
                .any(|p| s.starts_with(p))
        };
        assert!(is_container("docker://busybox"));
        assert!(is_container("docker-daemon:img"));
        assert!(is_container("oci:/path:tag"));
        assert!(!is_container("/local/path"));
        assert!(!is_container("./rel"));
    }

    #[test]
    fn get_pkgfs_src_errors_when_missing() {
        let dir = tempdir().unwrap();
        let ws = workspace_at(dir.path());
        assert!(get_pkgfs_src(&ws).is_err());
    }

    #[test]
    fn get_pkgfs_src_returns_directory() {
        let dir = tempdir().unwrap();
        let ws = workspace_at(dir.path());
        std::fs::create_dir(&ws.pkgfs).unwrap();
        assert_eq!(get_pkgfs_src(&ws).unwrap(), ws.pkgfs);
    }

    #[test]
    fn get_pkgfs_src_follows_symlink() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("real_src");
        std::fs::create_dir(&target).unwrap();
        let ws = workspace_at(dir.path());
        symlink(&target, &ws.pkgfs).unwrap();
        assert_eq!(get_pkgfs_src(&ws).unwrap(), target.canonicalize().unwrap());
    }

    #[test]
    fn get_pkgfs_src_rejects_plain_file() {
        let dir = tempdir().unwrap();
        let ws = workspace_at(dir.path());
        std::fs::write(&ws.pkgfs, b"x").unwrap();
        assert!(get_pkgfs_src(&ws).is_err());
    }

    #[test]
    fn prepare_creates_pkgfs_dir_when_no_src() {
        let dir = tempdir().unwrap();
        let ws = workspace_at(dir.path());
        prepare(&ws, None, None).unwrap();
        assert!(ws.pkgfs.is_dir());
    }

    #[test]
    fn prepare_symlinks_local_src() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir(&src).unwrap();
        let ws = workspace_at(dir.path());
        prepare(&ws, Some(src.to_str().unwrap()), None).unwrap();
        assert!(ws.pkgfs.is_symlink());
        assert_eq!(
            ws.pkgfs.canonicalize().unwrap(),
            src.canonicalize().unwrap()
        );
    }

    #[test]
    fn prepare_overwrites_existing_pkgfs_with_local_src() {
        let dir = tempdir().unwrap();
        let ws = workspace_at(dir.path());
        std::fs::create_dir(&ws.pkgfs).unwrap(); // pre-existing pkgfs
        let src = dir.path().join("src");
        std::fs::create_dir(&src).unwrap();
        prepare(&ws, Some(src.to_str().unwrap()), None).unwrap();
        assert!(ws.pkgfs.is_symlink());
    }

    #[test]
    fn prepare_dispatches_docker_src_to_skopeo() {
        let dir = tempdir().unwrap();
        let mock = MockCommandRunner::new();
        let handle = mock.clone();
        let ws = crate::Workspace::new(dir.path(), Box::new(mock));
        // Fails later (no real umoci output on disk), but the docker branch must
        // reach skopeo.
        let _ = prepare(&ws, Some("docker://busybox"), None);
        assert!(handle.calls().iter().any(|c| c.cmd == "skopeo"));
    }

    #[test]
    fn build_image_returns_image_file_as_is() {
        let dir = tempdir().unwrap();
        let img = dir.path().join("image.bin");
        std::fs::write(&img, b"img").unwrap();
        let ws = workspace_at(dir.path());
        symlink(&img, &ws.pkgfs).unwrap(); // pkgfs points at an image file
        let out = build_image(&ws, None).unwrap();
        assert_eq!(out, img.canonicalize().unwrap());
    }

    #[test]
    fn build_image_errors_when_pkgfs_missing() {
        let dir = tempdir().unwrap();
        let ws = workspace_at(dir.path());
        assert!(build_image(&ws, None).is_err());
    }
}
