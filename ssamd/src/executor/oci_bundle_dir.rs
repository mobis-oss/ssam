// Copyright 2025-2026 Hyundai Mobis Co., Ltd.
// SPDX-License-Identifier: Apache-2.0

use std::path::{Path, PathBuf};

use anyhow::Context as _;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OciBundleDir {
    path: PathBuf,
}

impl OciBundleDir {
    pub(crate) fn new(
        oci_bundle_root: &Path,
        package_name: &str,
        version: &str,
    ) -> anyhow::Result<Self> {
        let dir_name = format!("{package_name}_{version}");
        anyhow::ensure!(
            crate::utils::is_safe_path_segment(&dir_name),
            "Unsafe bundle directory name rejected: {dir_name:?}"
        );
        Ok(Self {
            path: oci_bundle_root.join(dir_name),
        })
    }

    pub(crate) fn ensure(&self) -> anyhow::Result<()> {
        if crate::mount::is_exact_mount(&self.pkgfs_path()) {
            return Ok(());
        }

        crate::utils::remove_path_if_exists(&self.path)?;
        // Atomic mkdir refuses a replacement symlink instead of following it.
        crate::utils::make_directory(&self.path, false)
            .with_context(|| format!("Failed to create bundle directory {}", self.path.display()))
    }

    pub(crate) fn as_path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn pkgfs_path(&self) -> PathBuf {
        self.path.join("pkgfs")
    }

    pub(crate) fn cleanup(&self) -> anyhow::Result<()> {
        crate::utils::remove_path_if_exists(&self.path)
    }
}

impl AsRef<Path> for OciBundleDir {
    fn as_ref(&self) -> &Path {
        self.as_path()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn create_oci_bundle_dir(
        oci_bundle_root: &Path,
        package_name: &str,
        version: &str,
    ) -> anyhow::Result<OciBundleDir> {
        let oci_bundle_dir = OciBundleDir::new(oci_bundle_root, package_name, version)?;
        oci_bundle_dir.ensure()?;
        Ok(oci_bundle_dir)
    }

    #[test]
    fn create_separates_hyphenated_name_and_version() {
        let root = tempfile::tempdir().expect("bundle root should be created");
        for (package_name, version, expected) in [
            ("abc-1.0.0", "1.0.0", "abc-1.0.0_1.0.0"),
            ("abc", "1.0.0-rc.1", "abc_1.0.0-rc.1"),
        ] {
            let oci_bundle_dir = create_oci_bundle_dir(root.path(), package_name, version)
                .expect("bundle directory should be created");
            assert_eq!(oci_bundle_dir.as_path(), root.path().join(expected));
            oci_bundle_dir
                .cleanup()
                .expect("bundle directory should be removed");
        }
    }

    #[test]
    fn ensure_recreates_directory_after_cleanup() {
        let root = tempfile::tempdir().expect("bundle root should be created");
        let oci_bundle_dir =
            OciBundleDir::new(root.path(), "abc", "1.0.0").expect("bundle path should be valid");

        oci_bundle_dir
            .ensure()
            .expect("bundle directory should be created");
        oci_bundle_dir
            .cleanup()
            .expect("bundle directory should be removed");
        oci_bundle_dir
            .ensure()
            .expect("bundle directory should be recreated");
        assert!(oci_bundle_dir.as_path().is_dir());
    }

    #[test]
    fn bundle_path_remains_until_explicit_cleanup() {
        let root = tempfile::tempdir().expect("bundle root should be created");
        let path = create_oci_bundle_dir(root.path(), "abc", "1.0.0")
            .expect("bundle directory should be created");

        assert!(path.as_path().exists());
        path.cleanup()
            .expect("bundle directory should be removed explicitly");
    }

    #[test]
    fn create_requires_existing_oci_bundle_root() {
        let temp_dir = tempfile::tempdir().expect("bundle root parent should be created");
        let root = temp_dir.path().join("bundle");

        let oci_bundle_dir =
            OciBundleDir::new(&root, "abc", "1.0.0").expect("bundle path should be valid");
        let result = oci_bundle_dir.ensure();

        assert!(result.is_err(), "missing bundle root must be rejected");
        assert!(!root.exists(), "bundle creation must not create the root");
    }

    #[test]
    fn create_removes_stale_symlink_without_following_target() {
        let root = tempfile::tempdir().expect("bundle root should be created");
        let target = root.path().join("target");
        fs::create_dir(&target).expect("target directory should be created");
        let marker = target.join("keep");
        fs::write(&marker, b"keep").expect("target marker should be created");

        let bundle_path = root.path().join("abc_1.0.0");
        std::os::unix::fs::symlink(&target, &bundle_path).expect("stale symlink should be created");

        let oci_bundle_dir =
            OciBundleDir::new(root.path(), "abc", "1.0.0").expect("bundle path should be valid");
        oci_bundle_dir
            .ensure()
            .expect("bundle directory should be created");

        assert!(marker.exists(), "symlink target must not be removed");
        assert!(
            fs::symlink_metadata(&bundle_path)
                .expect("bundle path should exist")
                .file_type()
                .is_dir(),
            "bundle path must be a directory, not a symlink"
        );
    }

    #[test]
    fn cleanup_removes_stale_symlink_without_following_target() {
        let root = tempfile::tempdir().expect("bundle root should be created");
        let target = root.path().join("target");
        fs::create_dir(&target).expect("target directory should be created");
        let marker = target.join("keep");
        fs::write(&marker, b"keep").expect("target marker should be created");

        let bundle_path = root.path().join("abc_1.0.0");
        std::os::unix::fs::symlink(&target, &bundle_path).expect("stale symlink should be created");

        OciBundleDir {
            path: bundle_path.clone(),
        }
        .cleanup()
        .expect("stale symlink should be removed");

        assert!(marker.exists(), "symlink target must not be removed");
        assert!(!bundle_path.exists(), "stale symlink must be removed");
    }
}
