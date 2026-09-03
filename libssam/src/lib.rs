// Copyright 2025-2026 Hyundai Mobis Co., Ltd.
// SPDX-License-Identifier: Apache-2.0

pub mod ssam_package;
// tonic-generated code; pedantic lints suppressed
#[allow(clippy::pedantic)]
pub mod remocon {
    pub const CONTROL_SOCKET_PATH: &str = "\0ssamd";
    tonic::include_proto!("ssamd.remocon");

    use crate::ssam_package::ssam_pkg_info::PackageInfoResult;
    use crate::ssam_package::ssam_pkg_metadata::PackageMetadata;

    pub const API_VERSION: i32 = ApiVersion::Current as i32;
    pub const API_VERSION_METADATA_KEY: &str = "x-ssam-api-version";

    pub fn api_version_string() -> String {
        API_VERSION.to_string()
    }

    impl From<&PackageMetadata> for PackageData {
        fn from(metadata: &PackageMetadata) -> Self {
            Self {
                package_name: metadata.get_package_name().to_owned(),
                version: metadata.get_package_version().to_owned(),
                description: metadata.package.description.clone(),
            }
        }
    }

    impl From<&PackageMetadata> for InstallPackageResponse {
        fn from(metadata: &PackageMetadata) -> Self {
            Self {
                package_data: Some(metadata.into()),
            }
        }
    }

    impl From<PackageInfoResult> for PackageInfo {
        fn from(result: PackageInfoResult) -> Self {
            let base = Self {
                success: true,
                name: String::new(),
                status: String::new(),
                package_data: None,
                quota: None,
                package_path: String::new(),
                error_summary: None,
                error_details: None,
            };

            match result {
                PackageInfoResult::Normal(info) => Self {
                    name: info.package_name,
                    status: info.package_status,
                    package_data: Some((&info.package_metadata).into()),
                    quota: Some(QuotaInformation {
                        enabled: info.quota_info.enabled,
                        limit: info.quota_info.limit,
                    }),
                    package_path: info.package_path,
                    ..base
                },
                PackageInfoResult::Broken { filename, info } => Self {
                    name: filename,
                    status: "Broken".to_owned(),
                    package_path: info.package_path,
                    error_summary: Some(info.broken_info.summary),
                    error_details: Some(info.broken_info.details),
                    ..base
                },
                PackageInfoResult::NotFound { package_name } => Self {
                    success: false,
                    name: package_name.clone(),
                    status: "NotFound".to_owned(),
                    package_path: String::new(),
                    error_summary: Some(format!("Package not found: {package_name}")),
                    ..base
                },
            }
        }
    }
}

pub use remocon::{
    InspectLocalPackageResult, InstallPackageResponse, ListPackageInfoFromFilesRequest,
    ListPackageInfoRequest, ListPackageInfoResponse, ListPackagesResponse, PackageActionResult,
    PackageData, PackageInfo, PackageStatusEntry, QuotaInformation, StartPackageResponse,
    StopPackageResponse, TimelineData, TimelineEvent, TimelineEventKind, WipePackageDataResponse,
};
pub use ssam_package::PackageParseError;

#[cfg(test)]
mod remocon_tests {
    use super::*;
    use crate::ssam_package::PackageFsVerityInfo;
    use crate::ssam_package::ssam_pkg_info::{
        BrokenPackageInfo, BrokenReason, PackageInfo as PackageStateInfo, PackageInfoResult,
        QuotaInformation,
    };
    use crate::ssam_package::ssam_pkg_metadata::PackageMetadata;
    use crate::superblock::FsType;

    fn test_metadata() -> PackageMetadata {
        PackageMetadata::new(
            crate::config::PackageConfigSpec {
                package: crate::config::Package {
                    name: "test-pkg".to_owned(),
                    version: "1.2.3".to_owned(),
                    description: "Test package".to_owned(),
                    autostart: Some(true),
                },
                container: crate::config::Container {
                    storage_limit: Some(1024),
                    data_dirs: Some("/app/data".to_owned()),
                    security: crate::config::Security {
                        seccomp: true,
                        mac: true,
                    },
                    network: Some(crate::config::Network {
                        mode: None,
                        bridge: None,
                    }),
                },
                service: crate::config::Service {
                    service_type: "notify".to_owned(),
                    bus_name: None,
                    remain_after_exit: Some(false),
                },
            },
            FsType::Erofs,
            PackageFsVerityInfo {
                data_size: 0,
                hash_size: 0,
                table_params: "1 4096 4096 100 101 sha256 mock_hash_root deadbeef".to_owned(),
                hash_offset: 0,
            },
        )
        .unwrap()
    }

    #[test]
    fn converts_normal_package_info() {
        let response = PackageInfo::from(PackageInfoResult::Normal(PackageStateInfo {
            package_name: "test-pkg".to_owned(),
            package_path: "/packages/test-pkg.ssam".to_owned(),
            package_metadata: test_metadata(),
            package_status: "Running".to_owned(),
            quota_info: QuotaInformation {
                enabled: true,
                limit: 2048,
            },
        }));

        assert!(response.success);
        assert_eq!(response.name, "test-pkg");
        assert_eq!(response.status, "Running");
        assert_eq!(response.package_path, "/packages/test-pkg.ssam");
        let package_data = response.package_data.expect("normal package data");
        assert_eq!(package_data.package_name, "test-pkg");
        assert_eq!(package_data.version, "1.2.3");
        assert_eq!(package_data.description, "Test package");
        let quota = response.quota.expect("normal package quota");
        assert!(quota.enabled);
        assert_eq!(quota.limit, 2048);
    }

    #[test]
    fn converts_not_found_package_info() {
        let response = PackageInfo::from(PackageInfoResult::NotFound {
            package_name: "missing-pkg".to_owned(),
        });

        assert!(!response.success);
        assert_eq!(response.name, "missing-pkg");
        assert_eq!(response.status, "NotFound");
        assert!(
            response
                .error_summary
                .as_deref()
                .is_some_and(|summary| summary.contains("missing-pkg"))
        );
    }

    #[test]
    fn converts_broken_package_info() {
        let response = PackageInfo::from(PackageInfoResult::Broken {
            filename: "broken-pkg".to_owned(),
            info: BrokenPackageInfo {
                package_path: "/packages/broken-pkg.ssam".to_owned(),
                broken_info: BrokenReason::new("Parse error", "Invalid package metadata"),
            },
        });

        assert!(response.success);
        assert_eq!(response.name, "broken-pkg");
        assert_eq!(response.status, "Broken");
        assert_eq!(response.package_path, "/packages/broken-pkg.ssam");
        assert!(response.package_data.is_none());
        assert_eq!(response.error_summary.as_deref(), Some("Parse error"));
        assert_eq!(
            response.error_details.as_deref(),
            Some("Invalid package metadata")
        );
    }
}

/// Seccomp policy module providing default security profile for containers.
pub mod seccomp {
    include!(concat!(env!("OUT_DIR"), "/default_seccomp_policy.rs"));
}

pub mod config {
    // To see the generated struct while in VSCode,
    // # Ctrl + Shift + P
    // # Choose 'rust_analyzer: Expand macro recursively at caret'
    use proc_macros::toml_file_to_struct;
    toml_file_to_struct!("data/package_config_spec.toml");

    /// Decode ceiling for every `.ssam` bincode decode. Bounds pre-verification
    /// allocation: an untrusted package's oversized container length prefix would
    /// otherwise allocate unbounded memory and abort root ssamd. Applies to decode
    /// only (bincode ignores it on encode), so the wire format is unchanged. The
    /// largest decoded structure is far smaller; raise this if one ever grows.
    pub const SSAM_MAX_DECODE_SIZE: usize = 1024 * 1024;

    // Whether Fixint or Varint is the difference between legacy and standard
    pub static SSAM_SERIALIZATION_CONFIG: bincode::config::Configuration<
        bincode::config::LittleEndian,
        bincode::config::Fixint,
        bincode::config::Limit<SSAM_MAX_DECODE_SIZE>,
    > = bincode::config::legacy().with_limit::<SSAM_MAX_DECODE_SIZE>();
}

pub mod container {
    use serde::{Deserialize, Serialize};
    use strum_macros::{Display, EnumString, IntoStaticStr, VariantNames};
    #[derive(
        Debug, Clone, Copy, Serialize, EnumString, Deserialize, VariantNames, IntoStaticStr, Display,
    )]
    #[strum(serialize_all = "lowercase")]
    pub enum ContainerServiceType {
        Simple,
        Exec,
        Forking,
        Oneshot,
        DBus,
        Notify,
        Idle,
    }

    #[derive(Debug, Clone, PartialEq, Eq, EnumString)]
    #[strum(serialize_all = "lowercase")]
    pub enum NetworkMode {
        Host,
        None,
        Bridge,
        /// Join a sysadmin-provisioned netns at this path. Not a keyword, so
        /// excluded from strum's string parsing (see `parse_mode`).
        #[strum(disabled)]
        Netns(std::path::PathBuf),
    }

    impl NetworkMode {
        /// Parse the `[container.network] mode` value: an absolute path selects an
        /// external pre-existing netns; otherwise it is a keyword (host/none/bridge).
        ///
        /// # Errors
        ///
        /// Returns an error if `s` is not an absolute path and does not match a
        /// known keyword (`host`, `none`, `bridge`).
        pub fn parse_mode(s: &str) -> Result<Self, strum::ParseError> {
            if std::path::Path::new(s).is_absolute() {
                Ok(NetworkMode::Netns(std::path::PathBuf::from(s)))
            } else {
                s.parse()
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn parse_mode_recognizes_keywords_and_absolute_path() {
            assert_eq!(NetworkMode::parse_mode("host").unwrap(), NetworkMode::Host);
            assert_eq!(
                NetworkMode::parse_mode("bridge").unwrap(),
                NetworkMode::Bridge
            );
            assert_eq!(
                NetworkMode::parse_mode("/run/netns/foo").unwrap(),
                NetworkMode::Netns(std::path::PathBuf::from("/run/netns/foo"))
            );
            assert!(NetworkMode::parse_mode("garbage").is_err());
            // Relative path (no leading '/') falls into keyword parsing and fails.
            assert!(NetworkMode::parse_mode("./rel").is_err());
        }
    }
}

pub mod superblock;

pub mod utils {
    use anyhow::Context;
    use std::path::Path;

    pub trait PrettyJsonWriter {
        /// Serializes `self` as pretty-printed JSON and writes it to `path`,
        /// creating or truncating the file as necessary.
        ///
        /// # Errors
        ///
        /// Returns an error if the file cannot be created or the JSON serialization fails.
        fn save_pretty<P: AsRef<Path>>(&self, path: P) -> anyhow::Result<()>;
    }

    impl PrettyJsonWriter for oci_spec::runtime::Spec {
        fn save_pretty<P: AsRef<Path>>(&self, path: P) -> anyhow::Result<()> {
            let file = std::fs::File::create(path)?;
            serde_json::to_writer_pretty(file, self).context("Failed to write spec to file")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_package_conf(invalid: bool) -> &'static str {
        // Create package config file
        if invalid {
            r#"
                [package]
                name = "test_package"
                autostart = true

                [container]
                storage_limit = 2000
                data_dirs = "/app/data:/app/logs"

                [service]
                service_type = "notify"
                bus_name = "com.test.service"
                remain_after_exit = false
            "#
        } else {
            r#"
                [package]
                name = "test_package"
                autostart = true
                version = "0.0.1"
                description = "A test package"

                [container]
                storage_limit = 2000
                data_dirs = "/app/data:/app/logs"

                [container.security]
                seccomp = true
                mac = true

                [container.network]

                [service]
                service_type = "notify"
                bus_name = "com.test.service"
                remain_after_exit = false
            "#
        }
    }

    #[test]
    fn test_parse_package_config() {
        let package_config: Result<config::PackageConfigSpec, toml::de::Error> =
            toml::from_str(create_test_package_conf(false));
        assert!(package_config.is_ok());

        let package_config: Result<config::PackageConfigSpec, toml::de::Error> =
            toml::from_str(create_test_package_conf(true));
        assert!(package_config.is_err());
    }

    fn create_legacy_package_conf_with_port_mappings() -> &'static str {
        r#"
            [package]
            name = "test_package"
            version = "0.0.1"
            description = "A test package"

            [container]
            storage_limit = 2000
            data_dirs = "/app/data:/app/logs"

            [container.security]
            seccomp = true
            mac = true

            [container.network]
            mode = "bridge"

            [container.network.bridge]
            network_name = "shared-net-a"
            interface_name = "eth0"
            port_mappings = ["8080:80", "8443:443/tcp", "5353:53/udp"]

            [service]
            service_type = "notify"
            bus_name = "com.test.service"
            remain_after_exit = false
        "#
    }

    #[test]
    fn test_parse_package_config_rejects_legacy_port_mappings() {
        let package_config: Result<config::PackageConfigSpec, toml::de::Error> =
            toml::from_str(create_legacy_package_conf_with_port_mappings());
        assert!(package_config.is_err());
    }
}
