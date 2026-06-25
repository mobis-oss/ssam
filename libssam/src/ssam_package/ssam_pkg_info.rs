// Copyright 2025-2026 Hyundai Mobis Co., Ltd.
// SPDX-License-Identifier: Apache-2.0

pub use super::ssam_pkg_metadata::PackageMetadata;

use std::fmt;

use super::error::PackageParseError;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BrokenReason {
    pub summary: String,
    pub details: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BrokenPackageInfo {
    pub package_path: String,
    pub broken_info: BrokenReason,
}

impl fmt::Display for BrokenPackageInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if f.alternate() {
            write!(
                f,
                "Path: {}\nReason: {:#}",
                self.package_path, self.broken_info
            )
        } else {
            write!(
                f,
                "Path: {}\nReason: {}",
                self.package_path, self.broken_info
            )
        }
    }
}

impl BrokenReason {
    pub fn new(summary: impl Into<String>, details: impl Into<String>) -> Self {
        Self {
            summary: summary.into(),
            details: details.into(),
        }
    }
}

impl From<&anyhow::Error> for BrokenReason {
    fn from(error: &anyhow::Error) -> Self {
        let (summary, details) = if let Some(parse_err) = error.downcast_ref::<PackageParseError>()
        {
            // PackageParseError uses alternate fmt for details
            (parse_err.to_string(), format!("{parse_err:#}"))
        } else {
            (error.to_string(), format!("{error:?}"))
        };

        Self { summary, details }
    }
}

impl fmt::Display for BrokenReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if f.alternate() {
            write!(f, "{}", self.details)
        } else {
            write!(f, "{}", self.summary)
        }
    }
}

// Allow large size difference between variants because:
// 1. PackageInfoResult is short-lived - created, serialized via gRPC, then dropped
// 2. Normal variant is the common case (99%+), Broken is rare
// 3. Avoids heap allocation overhead from Box
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, strum::Display)]
pub enum PackageInfoResult {
    #[strum(to_string = "{0}")]
    Normal(PackageInfo),
    #[strum(to_string = "Broken Package: {filename}\n{info:#}")]
    Broken {
        filename: String,
        info: BrokenPackageInfo,
    },
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct QuotaInformation {
    pub enabled: bool,
    pub limit: u64,
}

impl fmt::Display for QuotaInformation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Quota: enabled={}, limit={}KB", self.enabled, self.limit)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct PackageInfo {
    pub package_name: String,
    pub package_path: String,
    pub package_metadata: PackageMetadata,
    pub package_status: String,
    pub quota_info: QuotaInformation,
}

impl fmt::Display for PackageInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Package name
        writeln!(f, "Package Name: {}", self.package_name)?;

        // Package status
        writeln!(f, "Status: {}", self.package_status)?;

        // Package config (TOML pretty string)
        let config_toml = toml::to_string_pretty(&self.package_metadata).map_err(|_| fmt::Error)?;
        writeln!(f, "Configuration:")?;
        writeln!(f, "---")?;
        for line in config_toml.lines() {
            writeln!(f, "    {line}")?;
        }
        writeln!(f, "---")?;

        // Quota information
        write!(f, "{}", self.quota_info)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Container, Network, Package, Security, Service};
    use crate::ssam_package::{PackageConfigSpec, PackageFsVerityInfo};

    #[test]
    fn test_quota_information_display() {
        let quota = QuotaInformation {
            enabled: false,
            limit: 1024,
        };
        let output = format!("{quota}");
        assert_eq!(output, "Quota: enabled=false, limit=1024KB");
    }

    #[test]
    fn test_package_information_display() {
        let package_config = PackageConfigSpec {
            package: Package {
                name: "test-package".to_string(),
                autostart: Some(true),
                version: "1.0.0".to_string(),
                description: "A test package".to_string(),
            },
            container: Container {
                storage_limit: Some(2048),
                data_dirs: Some("/app/data:/var/lib/app".to_string()),
                security: Security {
                    seccomp: true,
                    mac: true,
                },
                network: Some(Network {
                    mode: None,
                    bridge: None,
                }),
            },
            service: Service {
                service_type: "notify".to_string(),
                bus_name: Some("org.example.test".to_string()),
                remain_after_exit: Some(false),
            },
        };
        let package_metadata = PackageMetadata::new(
            package_config.clone(),
            crate::superblock::FsType::Erofs,
            PackageFsVerityInfo {
                data_size: 0,
                hash_size: 0,
                root_hash: "dummy_hash_root".to_string(),
                hash_offset: 0,
            },
        )
        .unwrap();
        let quota_info = QuotaInformation {
            enabled: true,
            limit: 2048,
        };
        let package_info = PackageInfo {
            package_name: "test-package".to_string(),
            package_path: "/tmp/test-package.ssam".to_string(),
            package_metadata,
            package_status: "Running".to_string(),
            quota_info,
        };
        let display_output = format!("{package_info}");
        assert!(display_output.contains("Package Name: test-package"));
        assert!(display_output.contains("Status: Running"));
        assert!(display_output.contains("Configuration:"));
        assert!(display_output.contains("Quota: enabled=true, limit=2048KB"));
    }

    #[test]
    fn test_broken_info() {
        // Test new
        let info = BrokenReason::new("Summary", "Detailed error message");
        assert_eq!(info.summary, "Summary");
        assert_eq!(info.details, "Detailed error message");

        // Test Display
        assert_eq!(format!("{info}"), "Summary");
        assert_eq!(format!("{info:#}"), "Detailed error message");

        // Test From<&anyhow::Error> with PackageParseError
        let parse_err = PackageParseError::InvalidMagic;

        let summary = parse_err.to_string();
        let details = format!("{parse_err:#}");
        let anyhow_err = anyhow::Error::from(parse_err);
        let info_from_anyhow = BrokenReason::from(&anyhow_err);

        assert_eq!(info_from_anyhow.summary, summary);
        assert_eq!(info_from_anyhow.details, details);

        // Test From<&anyhow::Error> with generic error
        let generic_err = anyhow::anyhow!("Something went wrong");
        let info_from_generic = BrokenReason::from(&generic_err);
        assert_eq!(info_from_generic.summary, "Something went wrong");
        assert!(info_from_generic.details.contains("Something went wrong"));

        // Test From<&anyhow::Error> with SignatureVerificationFailed
        let parse_err_sig = PackageParseError::SignatureVerificationFailed;

        let summary = parse_err_sig.to_string();
        let details = format!("{parse_err_sig:#}");
        let anyhow_sig = anyhow::Error::from(parse_err_sig);
        let info_from_sig = BrokenReason::from(&anyhow_sig);

        assert_eq!(info_from_sig.summary, summary);
        assert_eq!(info_from_sig.details, details);

        // Test InvalidFormatVersion with details
        let parse_err_version = PackageParseError::InvalidFormatVersion {
            expected: "0.1.0".to_string(),
            actual: "9.9.9".to_string(),
        };

        let summary = parse_err_version.to_string();
        let details = format!("{parse_err_version:#}");
        let anyhow_version = anyhow::Error::from(parse_err_version);
        let info_from_version = BrokenReason::from(&anyhow_version);

        assert_eq!(info_from_version.summary, summary);
        assert_eq!(info_from_version.details, details);
    }

    #[test]
    fn test_package_info_result() {
        // Setup PackageInfo for Normal variant
        let package_config = PackageConfigSpec {
            package: Package {
                name: "test-package".to_string(),
                autostart: Some(true),
                version: "1.0.0".to_string(),
                description: "A test package".to_string(),
            },
            container: Container {
                storage_limit: Some(2048),
                data_dirs: Some("/app/data:/var/lib/app".to_string()),
                security: Security {
                    seccomp: true,
                    mac: true,
                },
                network: Some(Network {
                    mode: None,
                    bridge: None,
                }),
            },
            service: Service {
                service_type: "notify".to_string(),
                bus_name: Some("org.example.test".to_string()),
                remain_after_exit: Some(false),
            },
        };
        let package_metadata = PackageMetadata::new(
            package_config,
            crate::superblock::FsType::Erofs,
            PackageFsVerityInfo {
                data_size: 0,
                hash_size: 0,
                root_hash: "dummy_hash_root".to_string(),
                hash_offset: 0,
            },
        )
        .unwrap();
        let quota_info = QuotaInformation {
            enabled: true,
            limit: 2048,
        };
        let pkg_info = PackageInfo {
            package_name: "test-package".to_string(),
            package_path: "/tmp/test-package.ssam".to_string(),
            package_metadata,
            package_status: "Running".to_string(),
            quota_info,
        };

        // Test Normal variant
        let normal_result = PackageInfoResult::Normal(pkg_info.clone());
        let normal_display = format!("{normal_result}");
        assert!(normal_display.contains("Package Name: test-package"));
        assert!(normal_display.contains("Status: Running"));

        // Test Broken variant
        let broken_info = BrokenReason::new("Bad Header", "Header magic mismatch");
        let broken_package_info = BrokenPackageInfo {
            package_path: "/broken/bad_package.ssam".to_string(),
            broken_info,
        };

        assert_eq!(
            format!("{broken_package_info}"),
            "Path: /broken/bad_package.ssam\nReason: Bad Header"
        );
        assert_eq!(
            format!("{broken_package_info:#}"),
            "Path: /broken/bad_package.ssam\nReason: Header magic mismatch"
        );

        let broken_result = PackageInfoResult::Broken {
            filename: "bad_package.ssam".to_string(),
            info: broken_package_info,
        };
        let broken_display = format!("{broken_result}");

        // Expected format: "Broken Package: {filename}\n{info:#}"
        assert!(broken_display.contains("Broken Package: bad_package.ssam"));
        assert!(broken_display.contains("Path: /broken/bad_package.ssam"));
        assert!(broken_display.contains("Reason: Header magic mismatch"));
    }
}
