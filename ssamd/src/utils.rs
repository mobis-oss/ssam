// Copyright 2025-2026 Hyundai Mobis Co., Ltd.
// SPDX-License-Identifier: Apache-2.0

pub mod actor_supervisor {
    use std::any::type_name;
    use std::fmt::Display;

    use rsactor::{Actor, ActorRef, ActorResult};

    /// Policy defines ONLY the action on failure.
    /// Logging is the supervisor's (`spawn_with`) concern — NOT the policy's.
    pub trait FailurePolicy: Send + 'static {
        /// Action to take after failure is logged. Called only on failure.
        fn invoke();
    }

    pub struct ExitOnFailure;

    impl FailurePolicy for ExitOnFailure {
        fn invoke() {
            std::process::exit(1);
        }
    }

    pub struct IgnoreOnFailure;

    impl FailurePolicy for IgnoreOnFailure {
        fn invoke() {} // no-op: logging already done by supervisor
    }

    pub trait SupervisedActor: Actor + 'static
    where
        Self::Error: Display + Send,
    {
        type FailurePolicy: FailurePolicy;
    }

    pub fn spawn_with<A>(args: A::Args) -> ActorRef<A>
    where
        A: SupervisedActor,
        A::Error: Display + Send,
    {
        let (actor_ref, handle) = rsactor::spawn::<A>(args);
        tokio::spawn(async move {
            let result = handle.await;
            let name = type_name::<A>();
            match &result {
                Ok(ActorResult::Failed { error, phase, .. }) => {
                    log::error!("actor '{name}' failed in {phase}: {error}");
                    A::FailurePolicy::invoke();
                }
                Err(join_err) if join_err.is_panic() => {
                    log::error!("actor '{name}' panicked: {join_err}");
                    A::FailurePolicy::invoke();
                }
                _ => {
                    log::info!("actor '{name}' terminated");
                }
            }
        });
        actor_ref
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use rsactor::FailurePhase;
        use tokio::task::JoinError;

        struct TestActor;

        impl Actor for TestActor {
            type Args = ();
            type Error = anyhow::Error;

            async fn on_start((): Self::Args, _: &ActorRef<Self>) -> Result<Self, Self::Error> {
                Ok(Self)
            }
        }

        impl SupervisedActor for TestActor {
            type FailurePolicy = IgnoreOnFailure;
        }

        fn failed(phase: FailurePhase) -> ActorResult<TestActor> {
            ActorResult::Failed {
                actor: None,
                error: anyhow::anyhow!("simulated actor failure"),
                phase,
                killed: false,
            }
        }

        fn completed(killed: bool) -> ActorResult<TestActor> {
            ActorResult::Completed {
                actor: TestActor,
                killed,
            }
        }

        async fn panic_join_error() -> JoinError {
            tokio::spawn(async {
                panic!("simulated supervised-task panic");
            })
            .await
            .expect_err("a panicking task must resolve to a JoinError")
        }

        async fn cancelled_join_error() -> JoinError {
            let handle = tokio::spawn(std::future::pending::<()>());
            handle.abort();
            handle
                .await
                .expect_err("an aborted task must resolve to a JoinError")
        }

        fn is_failure<T: Actor>(result: &Result<ActorResult<T>, JoinError>) -> bool {
            match result {
                Ok(ActorResult::Failed { .. }) => true,
                Err(join_err) if join_err.is_panic() => true,
                _ => false,
            }
        }

        #[test]
        fn classifies_actor_failure_in_every_phase() {
            for phase in [
                FailurePhase::OnStart,
                FailurePhase::OnRun,
                FailurePhase::OnStop,
                FailurePhase::OnRunThenOnStop,
            ] {
                assert!(
                    is_failure::<TestActor>(&Ok(failed(phase))),
                    "ActorResult::Failed in {phase} must be treated as a failure"
                );
            }
        }

        #[test]
        fn classifies_completion_as_non_failure() {
            assert!(!is_failure::<TestActor>(&Ok(completed(false))));
            assert!(!is_failure::<TestActor>(&Ok(completed(true))));
        }

        #[tokio::test]
        async fn classifies_panic_as_failure() {
            assert!(is_failure::<TestActor>(&Err(panic_join_error().await)));
        }

        #[tokio::test]
        async fn classifies_cancellation_as_non_failure() {
            assert!(!is_failure::<TestActor>(&Err(cancelled_join_error().await)));
        }

        #[tokio::test]
        async fn spawn_with_returns_live_supervised_actor() {
            let actor_ref = spawn_with::<TestActor>(());
            actor_ref
                .stop()
                .await
                .expect("graceful stop of a supervised actor should succeed");
        }
    }
}

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
#[must_use]
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

/// True if `path` is a safe relative path: at least one component and every
/// component is `Normal`, so `..`, absolute root/prefix, a leading `.`, and
/// NUL are rejected. `components()` normalizes interior `.` away, so it can
/// never form an escaping segment. Rejects path traversal at trust boundaries.
/// Callers strip an optional single leading `/` before calling (`data_dirs`
/// are authored absolute-looking).
pub(crate) fn is_safe_relative_path(path: &std::path::Path) -> bool {
    use std::path::Component;
    let mut has_component = false;
    for c in path.components() {
        match c {
            Component::Normal(seg) => {
                if seg.as_encoded_bytes().contains(&0) {
                    return false;
                }
                has_component = true;
            }
            _ => return false,
        }
    }
    has_component
}

/// True if `name` is a safe single path segment (a package name): non-empty,
/// no `/`, no NUL, exactly one `Normal` component. Structural only — charset
/// policy is out of scope (owned by MLINUX-2095).
pub(crate) fn is_safe_path_segment(name: &str) -> bool {
    if name.is_empty() || name.contains('/') || name.contains('\0') {
        return false;
    }
    let mut comps = std::path::Path::new(name).components();
    matches!(comps.next(), Some(std::path::Component::Normal(_))) && comps.next().is_none()
}

pub(crate) mod quota_utils {
    use anyhow::Context;
    use linux_raw_sys::general::{FS_XFLAG_PROJINHERIT, fsxattr};
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

    /// Project ID of a directory with an active project quota, else `None`.
    ///
    /// Active requires PROJINHERIT (`P`) flag set AND projid != 0. ssamd sets
    /// both on quota grant; requiring both defends a crash between the two ioctls
    /// (P set, projid still 0) and stops adopting projid 0 (ext4 root inode).
    pub(crate) fn get_active_projid_inner(
        path: impl AsRef<Path>,
        ops: &impl FsAttributeConfigurator,
    ) -> anyhow::Result<Option<usize>> {
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

        let has_proj_inherit = attr.fsx_xflags & FS_XFLAG_PROJINHERIT != 0;
        Ok((has_proj_inherit && attr.fsx_projid != 0).then_some(attr.fsx_projid as usize))
    }

    pub(crate) fn get_active_projid(path: impl AsRef<Path>) -> anyhow::Result<Option<usize>> {
        get_active_projid_inner(path, &DefaultFsAttributeConfigurator)
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

        fn xattr_with(xflags: u32, projid: u32) -> fsxattr {
            fsxattr {
                fsx_xflags: xflags,
                fsx_projid: projid,
                ..default_fsxattr_with_projid(0)
            }
        }

        #[test]
        fn test_get_active_projid_flag_set_and_nonzero() {
            let temp_dir = TempDir::new().unwrap();
            let test_file = temp_dir.path().join("test_file");
            std::fs::File::create(&test_file).unwrap();

            let ops = MockFsAttributeConfigurator::new()
                .with_get_xattr(Ok(xattr_with(FS_XFLAG_PROJINHERIT, 42)));
            let id = get_active_projid_inner(&test_file, &ops).unwrap();
            assert_eq!(id, Some(42));
        }

        #[test]
        fn test_get_active_projid_inactive_returns_none() {
            let temp_dir = TempDir::new().unwrap();
            let test_file = temp_dir.path().join("test_file");
            std::fs::File::create(&test_file).unwrap();

            // No P flag, projid 0: fresh/external directory.
            let ops = MockFsAttributeConfigurator::new().with_get_xattr(Ok(xattr_with(0, 0)));
            assert_eq!(get_active_projid_inner(&test_file, &ops).unwrap(), None);

            // No P flag but projid set: stale/foreign projid, not ours.
            let ops = MockFsAttributeConfigurator::new().with_get_xattr(Ok(xattr_with(0, 7)));
            assert_eq!(get_active_projid_inner(&test_file, &ops).unwrap(), None);

            // P flag set but projid 0: crash between the two ioctls.
            let ops = MockFsAttributeConfigurator::new()
                .with_get_xattr(Ok(xattr_with(FS_XFLAG_PROJINHERIT, 0)));
            assert_eq!(get_active_projid_inner(&test_file, &ops).unwrap(), None);
        }

        #[test]
        fn test_get_active_projid_errors_nonexistent_and_symlink() {
            let ops = MockFsAttributeConfigurator::new();

            // nonexistent
            let result = get_active_projid_inner("/nonexistent/path", &ops);
            assert!(result.is_err());

            // broken symlink
            let temp_dir = TempDir::new().unwrap();
            let target_path = temp_dir.path().join("broken_symlink");
            symlink(temp_dir.path().join("invalid/path/broken"), &target_path).unwrap();
            let result = get_active_projid_inner(&target_path, &ops);
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

            // get_xattr failure path in get_active_projid
            let temp_dir = TempDir::new().unwrap();
            let test_file = temp_dir.path().join("test_file");
            std::fs::File::create(&test_file).unwrap();

            let ops = MockFsAttributeConfigurator::new()
                .with_get_xattr(Err(anyhow::anyhow!("mock error")));
            let result = get_active_projid_inner(&test_file, &ops);
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
    fn test_is_safe_path_segment_accepts_simple_names() {
        for name in ["nginx", "pkg_v2.3", "my-package"] {
            assert!(is_safe_path_segment(name), "{name} should be safe");
        }
    }

    #[test]
    fn test_is_safe_path_segment_rejects_unsafe_names() {
        for name in [
            "",
            "../evil",
            "sub/../evil",
            "subdir/name",
            "/abs",
            "pkg/",
            "a\0b",
        ] {
            assert!(!is_safe_path_segment(name), "{name:?} should be unsafe");
        }
    }

    #[test]
    fn test_is_safe_relative_path_accepts_safe_paths() {
        for path in [
            Path::new("var/data"),
            Path::new("subdir"),
            Path::new("var/lib"),
        ] {
            assert!(is_safe_relative_path(path), "{path:?} should be safe");
        }
    }

    #[test]
    fn test_is_safe_relative_path_rejects_unsafe_paths() {
        for path in [
            Path::new("../etc"),
            Path::new("a/../b"),
            Path::new("/abs"),
            Path::new(""),
            Path::new("a\0b"),
        ] {
            assert!(!is_safe_relative_path(path), "{path:?} should be unsafe");
        }
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
