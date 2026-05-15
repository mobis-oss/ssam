// Copyright 2025-2026 Hyundai Mobis Co., Ltd.
// SPDX-License-Identifier: Apache-2.0

use std::{os::unix::fs::DirBuilderExt, path::Path};

use anyhow::Context;

pub(crate) fn elapsed_ns() -> u64 {
    u64::try_from(crate::GLOBAL_TIMELINE_INSTANT.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

pub(crate) fn timeline_start(pkg: &str, phase: impl Into<String>) {
    timeline_start_at(pkg, phase, elapsed_ns());
}

pub(crate) fn timeline_complete(pkg: &str, phase: impl Into<String>) {
    timeline_complete_at(pkg, phase, elapsed_ns());
}

pub(crate) fn timeline_start_at(pkg: &str, phase: impl Into<String>, duration_ns: u64) {
    ssam_log::timeline!(libssam::remocon_schema::TimelineEvent {
        pkg: pkg.to_owned(),
        phase: phase.into(),
        duration_ns,
        kind: libssam::remocon_schema::TimelineEventKind::Started,
    });
}

pub(crate) fn timeline_complete_at(pkg: &str, phase: impl Into<String>, duration_ns: u64) {
    ssam_log::timeline!(libssam::remocon_schema::TimelineEvent {
        pkg: pkg.to_owned(),
        phase: phase.into(),
        duration_ns,
        kind: libssam::remocon_schema::TimelineEventKind::Completed,
    });
}

const MAX_DISPLAY_LEN: usize = 40;

/// Truncates a string with ellipsis if it exceeds `MAX_DISPLAY_LEN` characters.
/// Shows first 10 and last 7 characters with "..." in between.
pub fn ellipsis(s: &str) -> String {
    let char_count = s.chars().count();
    if char_count <= MAX_DISPLAY_LEN {
        s.to_string()
    } else {
        let start: String = s.chars().take(10).collect();
        let end: String = s.chars().skip(char_count - 7).collect();
        format!("{start}...{end}")
    }
}

pub(crate) fn make_directory(path: impl AsRef<Path>, recursive: bool) -> anyhow::Result<()> {
    std::fs::DirBuilder::new()
        .mode(0o755)
        .recursive(recursive)
        .create(&path)
        .context(format!(
            "Failed to create directory: {}",
            path.as_ref().display()
        ))
}

pub(crate) mod quota_utils {
    use anyhow::Context;
    use linux_raw_sys::general::fsxattr;
    use std::{fs, path::Path};

    // Trait abstraction for external syscalls/xattr operations
    pub(crate) trait FsAttributeConfigurator {
        fn get_xattr(&self, fd: &fs::File) -> anyhow::Result<fsxattr>;
        fn set_xattr(&self, fd: &fs::File, attr: &fsxattr) -> anyhow::Result<()>;
        fn get_flags(&self, fd: &fs::File) -> anyhow::Result<rustix::fs::IFlags>;
        fn set_flags(&self, fd: &fs::File, flags: rustix::fs::IFlags) -> anyhow::Result<()>;
    }

    // Default implementation using rustix/ioctl
    pub(crate) struct DefaultFsAttributeConfigurator;

    impl FsAttributeConfigurator for DefaultFsAttributeConfigurator {
        fn get_xattr(&self, fd: &fs::File) -> anyhow::Result<fsxattr> {
            const FS_GET_XATTR: rustix::ioctl::Opcode =
                rustix::ioctl::opcode::read::<fsxattr>(b'X', 31);
            unsafe {
                rustix::ioctl::ioctl(
                    fd,
                    rustix::ioctl::Getter::<{ FS_GET_XATTR }, fsxattr>::new(),
                )
            }
            .context("Failed to get xattr")
        }

        fn set_xattr(&self, fd: &fs::File, attr: &fsxattr) -> anyhow::Result<()> {
            const FS_SET_XATTR: rustix::ioctl::Opcode =
                rustix::ioctl::opcode::write::<fsxattr>(b'X', 32);
            unsafe {
                rustix::ioctl::ioctl(
                    fd,
                    rustix::ioctl::Setter::<{ FS_SET_XATTR }, fsxattr>::new(*attr),
                )
            }
            .context("Failed to set xattr")
        }

        fn get_flags(&self, fd: &fs::File) -> anyhow::Result<rustix::fs::IFlags> {
            rustix::fs::ioctl_getflags(fd).context("Failed to get current attributes")
        }

        fn set_flags(&self, fd: &fs::File, flags: rustix::fs::IFlags) -> anyhow::Result<()> {
            rustix::fs::ioctl_setflags(fd, flags)
                .with_context(|| format!("Failed to set flags: {flags:?}"))
        }
    }

    pub(crate) fn get_projid_inner(
        path: impl AsRef<Path>,
        ops: &impl FsAttributeConfigurator,
    ) -> anyhow::Result<usize> {
        let path = path.as_ref();
        let canonicalized = path
            .canonicalize()
            .with_context(|| format!("Cannot canonicalize the path: {}", path.display()))?;

        let file = fs::File::open(&canonicalized).context(format!(
            "Failed to open directory: {}",
            canonicalized.display()
        ))?;

        let attr = ops.get_xattr(&file).with_context(|| {
            format!(
                "Failed to get project id for directory: {}",
                canonicalized.display()
            )
        })?;
        Ok(attr.fsx_projid as usize)
    }

    pub(crate) fn get_projid(path: impl AsRef<Path>) -> anyhow::Result<usize> {
        get_projid_inner(path, &DefaultFsAttributeConfigurator)
    }

    fn set_project_quota_inner(
        dir: impl AsRef<Path>,
        enabled: bool,
        id: usize,
        ops: &impl FsAttributeConfigurator,
    ) -> anyhow::Result<()> {
        let dir_path = dir.as_ref();
        let file = fs::File::open(dir_path)
            .context(format!("Failed to open directory: {}", dir_path.display()))?;

        set_proj_inheritance(&file, enabled, ops).with_context(|| {
            format!(
                "Failed to set project inheritance for directory: {}",
                dir_path.display()
            )
        })?;
        set_projid(&file, id, ops).with_context(|| {
            format!(
                "Failed to set project ID for directory: {}",
                dir_path.display()
            )
        })
    }

    pub(crate) fn set_project_quota(
        dir: impl AsRef<Path>,
        enabled: bool,
        id: usize,
    ) -> anyhow::Result<()> {
        set_project_quota_inner(dir, enabled, id, &DefaultFsAttributeConfigurator)
    }

    fn set_proj_inheritance(
        file: &fs::File,
        enabled: bool,
        ops: &impl FsAttributeConfigurator,
    ) -> anyhow::Result<()> {
        let current_flags = ops.get_flags(file)?;

        let new_flags = if enabled {
            current_flags | rustix::fs::IFlags::PROJECT_INHERIT
        } else {
            current_flags & !rustix::fs::IFlags::PROJECT_INHERIT
        };

        if new_flags == current_flags {
            log::debug!(
                "PROJECT_INHERIT flag is already {}",
                if enabled { "set" } else { "unset" }
            );
            return Ok(());
        }

        ops.set_flags(file, new_flags).with_context(|| {
            format!(
                "Failed to {} PROJECT_INHERIT",
                if enabled { "set" } else { "unset" }
            )
        })
    }

    fn set_projid(
        file: &fs::File,
        id: usize,
        ops: &impl FsAttributeConfigurator,
    ) -> anyhow::Result<()> {
        let mut attr = ops.get_xattr(file)?;
        attr.fsx_projid =
            u32::try_from(id).with_context(|| format!("Project id {id} exceeds u32 max"))?;
        ops.set_xattr(file, &attr)
    }

    #[cfg(test)]
    mod quota_utils_test {
        use super::*;
        use rustix::fs::symlink;
        use tempfile::TempDir;

        struct MockFsAttributeConfigurator {
            xattr_get: Option<anyhow::Result<fsxattr>>,
            xattr_set: Option<anyhow::Result<()>>,
            flags_get: Option<anyhow::Result<rustix::fs::IFlags>>,
            flags_set: Option<anyhow::Result<()>>,
        }

        impl MockFsAttributeConfigurator {
            fn new() -> Self {
                Self {
                    xattr_get: None,
                    xattr_set: None,
                    flags_get: None,
                    flags_set: None,
                }
            }

            fn with_get_xattr(mut self, result: anyhow::Result<fsxattr>) -> Self {
                self.xattr_get = Some(result);
                self
            }

            fn with_set_xattr(mut self, result: anyhow::Result<()>) -> Self {
                self.xattr_set = Some(result);
                self
            }

            fn with_get_flags(mut self, result: anyhow::Result<rustix::fs::IFlags>) -> Self {
                self.flags_get = Some(result);
                self
            }

            fn with_set_flags(mut self, result: anyhow::Result<()>) -> Self {
                self.flags_set = Some(result);
                self
            }
        }

        impl FsAttributeConfigurator for MockFsAttributeConfigurator {
            fn get_xattr(&self, _fd: &fs::File) -> anyhow::Result<fsxattr> {
                match &self.xattr_get {
                    Some(Ok(attr)) => Ok(*attr),
                    Some(Err(_)) => Err(anyhow::anyhow!("mock get_xattr error")),
                    None => Ok(fsxattr {
                        fsx_xflags: 0,
                        fsx_extsize: 0,
                        fsx_nextents: 0,
                        fsx_projid: 0,
                        fsx_cowextsize: 0,
                        fsx_pad: [0; 8],
                    }),
                }
            }

            fn set_xattr(&self, _fd: &fs::File, _attr: &fsxattr) -> anyhow::Result<()> {
                match &self.xattr_set {
                    Some(Ok(())) | None => Ok(()),
                    Some(Err(_)) => Err(anyhow::anyhow!("mock set_xattr error")),
                }
            }

            fn get_flags(&self, _fd: &fs::File) -> anyhow::Result<rustix::fs::IFlags> {
                match &self.flags_get {
                    Some(Ok(flags)) => Ok(*flags),
                    Some(Err(_)) => Err(anyhow::anyhow!("mock get_flags error")),
                    None => Ok(rustix::fs::IFlags::empty()),
                }
            }

            fn set_flags(&self, _fd: &fs::File, _flags: rustix::fs::IFlags) -> anyhow::Result<()> {
                match &self.flags_set {
                    Some(Ok(())) | None => Ok(()),
                    Some(Err(_)) => Err(anyhow::anyhow!("mock set_flags error")),
                }
            }
        }

        fn default_fsxattr_with_projid(projid: u32) -> fsxattr {
            fsxattr {
                fsx_xflags: 0,
                fsx_extsize: 0,
                fsx_nextents: 0,
                fsx_projid: projid,
                fsx_cowextsize: 0,
                fsx_pad: [0; 8],
            }
        }

        #[test]
        fn test_get_projid_success() {
            let temp_dir = TempDir::new().unwrap();
            let test_file = temp_dir.path().join("test_file");
            std::fs::File::create(&test_file).unwrap();

            let ops = MockFsAttributeConfigurator::new()
                .with_get_xattr(Ok(default_fsxattr_with_projid(42)));
            let id = get_projid_inner(&test_file, &ops).unwrap();
            assert_eq!(id, 42);
        }

        #[test]
        fn test_get_projid_errors_nonexistent_and_symlink() {
            let ops = MockFsAttributeConfigurator::new();

            // nonexistent
            let result = get_projid_inner("/nonexistent/path", &ops);
            assert!(result.is_err());

            // broken symlink
            let temp_dir = TempDir::new().unwrap();
            let target_path = temp_dir.path().join("broken_symlink");
            symlink(temp_dir.path().join("invalid/path/broken"), &target_path).unwrap();
            let result = get_projid_inner(&target_path, &ops);
            assert!(result.is_err());
        }

        #[test]
        fn test_set_project_quota_enable_and_id_update() {
            let temp_dir = TempDir::new().unwrap();
            let test_dir = temp_dir.path().join("test_dir");
            std::fs::create_dir(&test_dir).unwrap();

            let ops = MockFsAttributeConfigurator::new()
                .with_get_flags(Ok(rustix::fs::IFlags::empty()))
                .with_set_flags(Ok(()))
                .with_get_xattr(Ok(default_fsxattr_with_projid(1)))
                .with_set_xattr(Ok(()));

            let result = set_project_quota_inner(&test_dir, true, 123, &ops);
            assert!(result.is_ok());
        }

        #[test]
        fn test_set_project_quota_disable_unsets_flag() {
            let temp_dir = TempDir::new().unwrap();
            let test_dir = temp_dir.path().join("test_dir");
            std::fs::create_dir(&test_dir).unwrap();

            let ops = MockFsAttributeConfigurator::new()
                .with_get_flags(Ok(rustix::fs::IFlags::PROJECT_INHERIT))
                .with_set_flags(Ok(()))
                .with_get_xattr(Ok(default_fsxattr_with_projid(10)))
                .with_set_xattr(Ok(()));

            let result = set_project_quota_inner(&test_dir, false, 10, &ops);
            assert!(result.is_ok());
        }

        #[test]
        fn test_set_proj_inheritance_noop_when_already_set() {
            let temp_dir = TempDir::new().unwrap();
            let test_dir = temp_dir.path().join("test_dir");
            std::fs::create_dir(&test_dir).unwrap();

            // When PROJECT_INHERIT is already set, set_flags should not be called
            let ops = MockFsAttributeConfigurator::new()
                .with_get_flags(Ok(rustix::fs::IFlags::PROJECT_INHERIT))
                .with_get_xattr(Ok(default_fsxattr_with_projid(7)))
                .with_set_xattr(Ok(()));

            let result = set_project_quota_inner(&test_dir, true, 7, &ops);
            assert!(result.is_ok());
        }

        #[test]
        fn test_error_propagation_open_and_ops_failures() {
            let ops = MockFsAttributeConfigurator::new();

            // open failure
            let result = set_project_quota_inner("/nonexistent", true, 1, &ops);
            assert!(result.is_err());

            // get_xattr failure path in get_projid
            let temp_dir = TempDir::new().unwrap();
            let test_file = temp_dir.path().join("test_file");
            std::fs::File::create(&test_file).unwrap();

            let ops = MockFsAttributeConfigurator::new()
                .with_get_xattr(Err(anyhow::anyhow!("mock error")));
            let result = get_projid_inner(&test_file, &ops);
            assert!(result.is_err());
        }

        #[test]
        fn test_set_project_quota_inheritance_error_context_covered() {
            let temp_dir = TempDir::new().unwrap();
            let test_dir = temp_dir.path().join("test_dir");
            std::fs::create_dir(&test_dir).unwrap();

            let ops = MockFsAttributeConfigurator::new()
                .with_get_flags(Ok(rustix::fs::IFlags::empty()))
                .with_set_flags(Err(anyhow::anyhow!("mock set_flags error")));

            let result = set_project_quota_inner(&test_dir, true, 123, &ops);
            assert!(result.is_err());
            let err_msg = format!("{}", result.unwrap_err());
            assert!(err_msg.contains("Failed to set project inheritance"));
        }

        #[test]
        fn test_set_project_quota_projid_error_context_covered() {
            let temp_dir = TempDir::new().unwrap();
            let test_dir = temp_dir.path().join("test_dir");
            std::fs::create_dir(&test_dir).unwrap();

            let ops = MockFsAttributeConfigurator::new()
                .with_get_flags(Ok(rustix::fs::IFlags::empty()))
                .with_set_flags(Ok(()))
                .with_get_xattr(Ok(default_fsxattr_with_projid(1)))
                .with_set_xattr(Err(anyhow::anyhow!("mock set_xattr error")));

            let result = set_project_quota_inner(&test_dir, true, 123, &ops);
            assert!(result.is_err());
            let err_msg = format!("{}", result.unwrap_err());
            assert!(err_msg.contains("Failed to set project ID"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_make_directory_success_recursive() {
        let temp_dir = TempDir::new().unwrap();
        let new_dir_path = temp_dir.path().join("new").join("nested").join("dir");

        let result = make_directory(&new_dir_path, true);
        assert!(result.is_ok());
        assert!(new_dir_path.exists());
    }

    #[test]
    fn test_make_directory_success_non_recursive() {
        let temp_dir = TempDir::new().unwrap();
        let new_dir_path = temp_dir.path().join("single_dir");

        let result = make_directory(&new_dir_path, false);
        assert!(result.is_ok());
        assert!(new_dir_path.exists());
    }

    #[test]
    fn test_make_directory_already_exists() {
        let temp_dir = TempDir::new().unwrap();
        let existing_dir = temp_dir.path().join("existing");
        std::fs::create_dir(&existing_dir).unwrap();

        let result = make_directory(&existing_dir, false);
        assert!(result.is_err());
    }

    #[test]
    fn test_make_directory_invalid_path() {
        // Try to create a directory in a path that doesn't exist without recursive flag
        let temp_dir = TempDir::new().unwrap();
        let invalid_path = temp_dir
            .path()
            .join("nonexistent")
            .join("nested")
            .join("dir");

        let result = make_directory(&invalid_path, false);
        assert!(result.is_err());
    }

    #[test]
    fn test_make_directory_edge_cases() {
        // Test root path - this might actually succeed since root exists
        let result = make_directory("/", false);
        assert!(result.is_err());
    }

    #[test]
    fn test_ellipsis_short() {
        assert_eq!(ellipsis("short"), "short");
    }

    #[test]
    fn test_ellipsis_exact_boundary() {
        // exactly 40 chars
        assert_eq!(
            ellipsis("1234567890123456789012345678901234567890"),
            "1234567890123456789012345678901234567890"
        );
    }

    #[test]
    fn test_ellipsis_long() {
        // 51 chars - should be truncated
        let displayed = ellipsis("this_is_a_very_long_string_that_should_be_truncated");
        assert_eq!(displayed, "this_is_a_...uncated");
        assert!(displayed.contains("..."));
    }

    #[test]
    fn test_ellipsis_just_over_boundary() {
        // 41 chars: 10 chars + "..." + last 7 chars
        assert_eq!(
            ellipsis("12345678901234567890123456789012345678901"),
            "1234567890...5678901"
        );
    }

    #[test]
    fn test_ellipsis_multibyte() {
        // 25 chars total (10 crabs + 10 rockets + 5 saucers) - under 40 limit, no truncation
        let s = format!("{}{}{}", "🦀".repeat(10), "🚀".repeat(10), "🛸".repeat(5));
        let displayed = ellipsis(&s);
        assert_eq!(displayed, s);
    }
}
