// Copyright 2025-2026 Hyundai Mobis Co., Ltd.
// SPDX-License-Identifier: Apache-2.0

use once_cell::sync::OnceCell;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// `AppArmor` profile information.
#[derive(Debug, Clone, PartialEq)]
pub struct Profile {
    /// The name of the profile.
    pub name: String,
    /// The current mode of the profile.
    pub mode: ProfileMode,
}

/// The mode of an `AppArmor` profile.
#[derive(Debug, Clone, PartialEq)]
pub enum ProfileMode {
    /// Enforce mode: policy violations are rejected and logged.
    Enforce,
    /// Complain mode: policy violations are permitted but logged.
    Complain,
    /// Kill mode: policy violations cause the task to be killed.
    Kill,
    /// Unconfined mode: the task is not restricted by `AppArmor`.
    Unconfined,
    /// Unknown mode: the mode string was not recognized.
    Unknown(String),
}

impl From<&str> for ProfileMode {
    fn from(s: &str) -> Self {
        match s {
            "enforce" => ProfileMode::Enforce,
            "complain" => ProfileMode::Complain,
            "kill" => ProfileMode::Kill,
            "unconfined" => ProfileMode::Unconfined,
            _ => ProfileMode::Unknown(s.to_string()),
        }
    }
}

trait AppArmorBackend {
    fn check_param(&self, name: &str) -> io::Result<bool>;
    fn find_mountpoint(&self) -> io::Result<Option<PathBuf>>;
}

struct DefaultAppArmorBackend;

impl AppArmorBackend for DefaultAppArmorBackend {
    fn check_param(&self, name: &str) -> io::Result<bool> {
        let path = format!("/sys/module/apparmor/parameters/{name}");
        let content = fs::read_to_string(path)?;
        Ok(content.trim() == "Y")
    }

    fn find_mountpoint(&self) -> io::Result<Option<PathBuf>> {
        let mounts = procfs::mounts().map_err(io::Error::other)?;
        for mount in mounts {
            if mount.fs_vfstype == "securityfs" {
                let path = Path::new(&mount.fs_file).join("apparmor");
                if path.exists() {
                    return Ok(Some(path));
                }
            }
        }
        Ok(None)
    }
}

fn is_private_enabled<S: AppArmorBackend>(sys: &S) -> io::Result<bool> {
    match sys.check_param("available") {
        Ok(v) => Ok(v),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

fn is_enabled_internal<S: AppArmorBackend>(sys: &S) -> io::Result<bool> {
    // Check "enabled" parameter
    // C logic: param_check_enabled()
    // Returns 1 (true), 0 (false), or -errno.
    let enabled = match sys.check_param("enabled") {
        Ok(v) => v,
        Err(e) if e.kind() == io::ErrorKind::NotFound => false,
        // Real errors (EACCES, etc.) are propagated
        Err(e) => return Err(e),
    };

    // If enabled, we expect the mountpoint to exist.
    if enabled {
        let mounted = sys.find_mountpoint()?;
        // C returns 1 if both enabled and mounted, 0 if enabled but not mounted (errno=ENOENT)
        return Ok(mounted.is_some());
    }

    // If not enabled (or file missing), check "available" (private enabled)
    // C logic: if (rc < 1) { if (!is_private_enabled()) ... }
    let available = is_private_enabled(sys)?;

    if available {
        // Private mode.
        // C logic: private = true; ... aa_find_mountpoint ... if (rc == 0) { ... errno = EBUSY; return 0; }
        // AppArmor is available on private interfaces only, but we return Ok(false)
        // to indicate that public interface is not enabled.
        // In C, this would set errno=EBUSY and return 0.
        let _ = sys.find_mountpoint()?;
        return Ok(false);
    }

    // If we reached here:
    // - enabled was false/missing AND available was false/missing.
    // C logic sets errno to ECANCELED (if enabled=0) or ENOSYS (if enabled missing).
    // We return Ok(false) as AppArmor is not enabled (not an error, just disabled state).
    Ok(false)
}

/// Retrieve a list of all loaded `AppArmor` profiles.
///
/// This function reads the `profiles` file from the `AppArmor` securityfs mount point.
/// It parses each line to extract the profile name and its current mode.
///
/// # Returns
///
/// - `Ok(Vec<Profile>)`: A list of profiles if `AppArmor` is mounted and the profiles file exists.
/// - `Ok(vec![])`: If `AppArmor` is not mounted or the profiles file does not exist.
/// - `Err(io::Error)`: If an I/O error occurs while reading the profiles file.
///
/// # Errors
///
/// Returns an `io::Error` if reading the `AppArmor` profiles file fails.
pub fn get_profiles() -> io::Result<Vec<Profile>> {
    let ops = DefaultAppArmorBackend;
    let profiles_path = ops
        .find_mountpoint()?
        .map(|p| p.join("profiles"))
        .filter(|p| p.exists());

    profiles_path.map_or(Ok(vec![]), |path| {
        let content = fs::read_to_string(path)?;
        Ok(parse_profiles(&content))
    })
}

fn parse_profiles(content: &str) -> Vec<Profile> {
    content
        .lines()
        .filter_map(|line| {
            let idx = line.rfind(" (")?;
            if idx + 2 >= line.len() - 1 {
                return None;
            }
            let name = &line[..idx];
            let mode_str = &line[idx + 2..line.len() - 1];
            Some(Profile {
                name: name.to_string(),
                mode: ProfileMode::from(mode_str),
            })
        })
        .collect()
}

/// Retrieve a specific `AppArmor` profile by name.
///
/// # Arguments
///
/// * `name` - The name of the profile to find.
///
/// # Returns
///
/// - `Ok(Some(Profile))`: If the profile with the given name is found.
/// - `Ok(None)`: If the profile is not found.
/// - `Err(io::Error)`: If an error occurs while retrieving the profiles.
///
/// # Errors
///
/// Returns an `io::Error` if reading the `AppArmor` profiles file fails.
pub fn get_profile(name: &str) -> io::Result<Option<Profile>> {
    let profiles = get_profiles()?;
    Ok(profiles.into_iter().find(|p| p.name == name))
}

/// Determine if `AppArmor` is enabled.
///
/// Returns `Ok(true)` if `AppArmor` is enabled and available on public interface,
/// `Ok(false)` if disabled or only available on private interface,
/// `Err` if a real system error occurred (e.g., permission denied, out of memory).
///
/// This is a pure Rust implementation of `aa_is_enabled` from `libapparmor`.
///
/// The result is cached on the first successful call. Subsequent calls will return
/// the cached value. If an error occurs, the result is not cached and the check
/// will be retried on the next call.
///
/// # Errors
///
/// Returns `io::Error` for actual system errors:
/// - `PermissionDenied` (EACCES): No permission to access `AppArmor` parameters
/// - Other I/O errors from reading `/sys/module/apparmor/parameters/` or `/proc/mounts`
///
/// # Note
///
/// Unlike the C version which sets errno, this function returns:
/// - `Ok(true)`: `AppArmor` enabled and mounted (C returns 1)
/// - `Ok(false)`: `AppArmor` disabled, not present, or only on private interface (C returns 0 with errno)
/// - `Err(e)`: Actual system error occurred (C returns 0 with errno)
pub fn is_enabled() -> io::Result<bool> {
    static ENABLED: OnceCell<bool> = OnceCell::new();
    ENABLED
        .get_or_try_init(|| is_enabled_internal(&DefaultAppArmorBackend))
        .copied()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    struct MockAppArmorBackend {
        params: HashMap<String, bool>,
        param_errors: HashMap<String, io::ErrorKind>,
        mountpoint_exists: bool,
        mountpoint_error: Option<io::ErrorKind>,
    }

    impl MockAppArmorBackend {
        fn new() -> Self {
            Self {
                params: HashMap::new(),
                param_errors: HashMap::new(),
                mountpoint_exists: false,
                mountpoint_error: None,
            }
        }

        fn set_param(mut self, name: &str, value: bool) -> Self {
            self.params.insert(name.to_string(), value);
            self
        }

        fn set_param_error(mut self, name: &str, error: io::ErrorKind) -> Self {
            self.param_errors.insert(name.to_string(), error);
            self
        }

        fn set_mountpoint(mut self, exists: bool) -> Self {
            self.mountpoint_exists = exists;
            self
        }

        fn set_mountpoint_error(mut self, error: io::ErrorKind) -> Self {
            self.mountpoint_error = Some(error);
            self
        }
    }

    impl AppArmorBackend for MockAppArmorBackend {
        fn check_param(&self, name: &str) -> io::Result<bool> {
            if let Some(&error_kind) = self.param_errors.get(name) {
                return Err(io::Error::new(error_kind, format!("Error reading {name}")));
            }
            self.params
                .get(name)
                .copied()
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "Parameter not found"))
        }

        fn find_mountpoint(&self) -> io::Result<Option<PathBuf>> {
            if let Some(error_kind) = self.mountpoint_error {
                return Err(io::Error::new(error_kind, "Mountpoint error"));
            }
            if self.mountpoint_exists {
                Ok(Some(PathBuf::from("/sys/kernel/security/apparmor")))
            } else {
                Ok(None)
            }
        }
    }

    // ============================================================================
    // Test cases returning Ok(true) - AppArmor is enabled and available
    // ============================================================================

    #[test]
    fn test_enabled_and_mounted() {
        // C equivalent: enabled=Y, mountpoint found -> return 1
        let sys = MockAppArmorBackend::new()
            .set_param("enabled", true)
            .set_mountpoint(true);
        assert!(is_enabled_internal(&sys).unwrap());
    }

    // ============================================================================
    // Test cases returning Ok(false) - AppArmor is not available (not an error)
    // ============================================================================

    #[test]
    fn test_enabled_but_no_mount() {
        // C equivalent: enabled=Y, mountpoint not found -> return 0, errno=ENOENT
        let sys = MockAppArmorBackend::new().set_param("enabled", true);
        assert!(!is_enabled_internal(&sys).unwrap());
    }

    #[test]
    fn test_disabled_explicitly() {
        // C equivalent: enabled=N, available=N -> return 0, errno=ECANCELED
        let sys = MockAppArmorBackend::new()
            .set_param("enabled", false)
            .set_param("available", false);
        assert!(!is_enabled_internal(&sys).unwrap());
    }

    #[test]
    fn test_params_missing() {
        // C equivalent: enabled missing -> return 0, errno=ENOSYS
        let sys = MockAppArmorBackend::new();
        assert!(!is_enabled_internal(&sys).unwrap());
    }

    #[test]
    fn test_disabled_but_available_and_mounted() {
        // C equivalent: enabled=N, available=Y, mountpoint found -> return 0, errno=EBUSY
        // This represents private interface only (LSM stacking scenario)
        let sys = MockAppArmorBackend::new()
            .set_param("enabled", false)
            .set_param("available", true)
            .set_mountpoint(true);
        assert!(!is_enabled_internal(&sys).unwrap());
    }

    #[test]
    fn test_disabled_but_available_no_mount() {
        // C equivalent: enabled=N, available=Y, mountpoint not found -> return 0
        let sys = MockAppArmorBackend::new()
            .set_param("enabled", false)
            .set_param("available", true);
        assert!(!is_enabled_internal(&sys).unwrap());
    }

    #[test]
    fn test_enabled_missing_but_available() {
        // C equivalent: enabled missing, available=Y -> return 0
        // Private interface exists but public one doesn't
        let sys = MockAppArmorBackend::new()
            .set_param("available", true)
            .set_mountpoint(true);
        assert!(!is_enabled_internal(&sys).unwrap());
    }

    // ============================================================================
    // Test cases returning Err - Real system errors
    // ============================================================================

    #[test]
    fn test_enabled_param_permission_denied() {
        // C equivalent: open() failed with EACCES -> return 0, errno=EACCES
        // Real error: no permission to read enabled parameter
        let sys =
            MockAppArmorBackend::new().set_param_error("enabled", io::ErrorKind::PermissionDenied);
        let result = is_enabled_internal(&sys);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
    }

    #[test]
    fn test_available_param_permission_denied() {
        // Permission denied when checking available parameter
        // enabled is missing, so we check available, but it fails with EACCES
        let sys = MockAppArmorBackend::new()
            .set_param_error("available", io::ErrorKind::PermissionDenied);
        let result = is_enabled_internal(&sys);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
    }

    #[test]
    fn test_mountpoint_error() {
        // Error while checking mountpoint (e.g., /proc/mounts read failure)
        let sys = MockAppArmorBackend::new()
            .set_param("enabled", true)
            .set_mountpoint_error(io::ErrorKind::PermissionDenied);
        let result = is_enabled_internal(&sys);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
    }

    #[test]
    fn test_mountpoint_error_in_private_mode() {
        // Error while checking mountpoint in private mode
        let sys = MockAppArmorBackend::new()
            .set_param("enabled", false)
            .set_param("available", true)
            .set_mountpoint_error(io::ErrorKind::Other);
        let result = is_enabled_internal(&sys);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_profiles() {
        let content = "docker-default (enforce)\n\
                       /usr/sbin/cupsd (complain)\n\
                       /usr/lib/snapd/snap-confine (unconfined)\n\
                       /usr/bin/man (kill)\n\
                       unknown-mode (weird-mode)\n\
                       invalid-format\n\
                       empty-mode ()\n";

        let profiles = parse_profiles(content);

        assert_eq!(profiles.len(), 5);

        assert_eq!(profiles[0].name, "docker-default");
        assert_eq!(profiles[0].mode, ProfileMode::Enforce);

        assert_eq!(profiles[1].name, "/usr/sbin/cupsd");
        assert_eq!(profiles[1].mode, ProfileMode::Complain);

        assert_eq!(profiles[2].name, "/usr/lib/snapd/snap-confine");
        assert_eq!(profiles[2].mode, ProfileMode::Unconfined);

        assert_eq!(profiles[3].name, "/usr/bin/man");
        assert_eq!(profiles[3].mode, ProfileMode::Kill);

        assert_eq!(profiles[4].name, "unknown-mode");
        match &profiles[4].mode {
            ProfileMode::Unknown(s) => assert_eq!(s, "weird-mode"),
            _ => panic!("Expected Unknown mode"),
        }
    }

    #[test]
    fn test_enabled_param_other_io_error() {
        // Other I/O errors should also be propagated
        let sys = MockAppArmorBackend::new().set_param_error("enabled", io::ErrorKind::Other);
        let result = is_enabled_internal(&sys);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::Other);
    }
}
