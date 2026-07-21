// Copyright 2025-2026 Hyundai Mobis Co., Ltd.
// SPDX-License-Identifier: Apache-2.0

use std::io::Read;

use crate::{
    config::{PackageConfigSpec, SSAM_SERIALIZATION_CONFIG},
    superblock::FsType,
};
use anyhow::{Context, Result, anyhow};

use super::{PackageFsVerityInfo, PackageParseError};

#[derive(
    Debug,
    Clone,
    PartialEq,
    bincode::Encode,
    bincode::Decode,
    serde::Serialize,
    serde::Deserialize,
    derive_more::Deref,
)]
pub struct PackageMetadata {
    #[deref]
    package_config: PackageConfigSpec,
    build_date: String,
    pkgfs_type: FsType,
    pkgfs_verity_info: PackageFsVerityInfo,
}

impl PackageMetadata {
    /// Constructs a new `PackageMetadata` from a package config, filesystem type,
    /// and verity info, recording the current timestamp as the build date.
    ///
    /// # Errors
    ///
    /// Returns an error if `package_config` contains an invalid semver version string.
    pub fn new(
        package_config: PackageConfigSpec,
        pkgfs_type: FsType,
        pkgfs_verity_info: PackageFsVerityInfo,
    ) -> Result<Self> {
        let build_date = jiff::Timestamp::now().to_string();
        // To ensure the version is valid semver
        _ = semver::Version::parse(package_config.get_package_version()).with_context(|| {
            format!(
                "Package version '{}' is invalid",
                package_config.get_package_version()
            )
        })?;
        Ok(PackageMetadata {
            package_config,
            build_date,
            pkgfs_type,
            pkgfs_verity_info,
        })
    }

    /// Returns the parsed semver version of this package.
    ///
    /// # Panics
    ///
    /// Panics if the stored version string is not valid semver. This cannot
    /// happen in practice because [`PackageMetadata::new`] validates the
    /// version at construction time.
    #[must_use]
    pub fn version(&self) -> semver::Version {
        semver::Version::parse(&self.package_config.package.version)
            .expect("Package version should be a valid semver")
    }

    pub(crate) fn pkgfs_type(&self) -> &FsType {
        &self.pkgfs_type
    }

    pub(crate) fn pkgfs_verity_info(&self) -> &PackageFsVerityInfo {
        &self.pkgfs_verity_info
    }

    pub(crate) fn serialize(&self) -> Result<Vec<u8>> {
        Ok(bincode::encode_to_vec(self, SSAM_SERIALIZATION_CONFIG)?)
    }

    pub(crate) fn deserialize<R: Read>(src: &mut R) -> Result<Self, PackageParseError> {
        bincode::decode_from_std_read(src, SSAM_SERIALIZATION_CONFIG).map_err(|e| {
            PackageParseError::ParseFailed {
                source: anyhow!(e).context("Failed to decode metadata"),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Bridge, Network, PackageConfigSpec, Security};
    use crate::ssam_package::tests::make_verity;

    fn create_test_metadata() -> PackageMetadata {
        let package_config = PackageConfigSpec {
            package: crate::config::Package {
                name: "test_package".to_string(),
                description: "A test package".to_string(),
                version: "1.0.0".to_string(),
                autostart: Some(true),
            },
            container: crate::config::Container {
                storage_limit: Some(1000),
                data_dirs: Some("/test/path1:/test/path2".to_string()),
                security: Security {
                    seccomp: true,
                    mac: true,
                },
                network: Some(Network {
                    mode: None,
                    bridge: None,
                }),
            },
            service: crate::config::Service {
                service_type: "notify".to_string(),
                bus_name: Some("test.bus.name".to_string()),
                remain_after_exit: Some(false),
            },
        };

        PackageMetadata::new(
            package_config,
            FsType::Erofs,
            make_verity("test_hash_root", 0),
        )
        .unwrap()
    }

    #[test]
    fn container_network_bridge_interface_name_getter() {
        let absent = create_test_metadata();
        assert_eq!(absent.get_container_network_bridge_interface_name(), None);

        let package_config = PackageConfigSpec {
            package: crate::config::Package {
                name: "test_package".to_string(),
                description: "A test package".to_string(),
                version: "1.0.0".to_string(),
                autostart: Some(true),
            },
            container: crate::config::Container {
                storage_limit: Some(1000),
                data_dirs: Some("/test/path1:/test/path2".to_string()),
                security: Security {
                    seccomp: true,
                    mac: true,
                },
                network: Some(Network {
                    mode: Some("bridge".to_string()),
                    bridge: Some(Bridge {
                        network_name: None,
                        interface_name: Some("eth1".to_string()),
                        port_mappings: None,
                    }),
                }),
            },
            service: crate::config::Service {
                service_type: "notify".to_string(),
                bus_name: Some("test.bus.name".to_string()),
                remain_after_exit: Some(false),
            },
        };
        let configured = PackageMetadata::new(
            package_config,
            FsType::Erofs,
            make_verity("test_hash_root", 0),
        )
        .unwrap();
        assert_eq!(
            configured
                .get_container_network_bridge_interface_name()
                .map(String::as_str),
            Some("eth1")
        );
    }

    #[test]
    fn container_network_bridge_network_name_getter() {
        let absent = create_test_metadata();
        assert_eq!(absent.get_container_network_bridge_network_name(), None);

        let package_config = PackageConfigSpec {
            package: crate::config::Package {
                name: "test_package".to_string(),
                description: "A test package".to_string(),
                version: "1.0.0".to_string(),
                autostart: Some(true),
            },
            container: crate::config::Container {
                storage_limit: Some(1000),
                data_dirs: Some("/test/path1:/test/path2".to_string()),
                security: Security {
                    seccomp: true,
                    mac: true,
                },
                network: Some(Network {
                    mode: Some("bridge".to_string()),
                    bridge: Some(Bridge {
                        network_name: Some("shared-net".to_string()),
                        interface_name: None,
                        port_mappings: None,
                    }),
                }),
            },
            service: crate::config::Service {
                service_type: "notify".to_string(),
                bus_name: Some("test.bus.name".to_string()),
                remain_after_exit: Some(false),
            },
        };
        let configured = PackageMetadata::new(
            package_config,
            FsType::Erofs,
            make_verity("test_hash_root", 0),
        )
        .unwrap();
        assert_eq!(
            configured
                .get_container_network_bridge_network_name()
                .map(String::as_str),
            Some("shared-net")
        );
    }

    #[test]
    fn test_serialize_deserialize() {
        let original_metadata = create_test_metadata();

        let inner = original_metadata.serialize();
        assert!(inner.is_ok());
        let mut serialized = std::io::Cursor::new(inner.unwrap());

        let deserialized_metadata = PackageMetadata::deserialize(&mut serialized);
        assert!(deserialized_metadata.is_ok());
        let deserialized_metadata = deserialized_metadata.unwrap();

        assert_eq!(
            original_metadata.package_config.package.name,
            deserialized_metadata.package_config.package.name
        );
        assert_eq!(
            original_metadata.package_config.service.service_type,
            deserialized_metadata.package_config.service.service_type
        );
        assert_eq!(
            original_metadata.pkgfs_verity_info.table_params,
            deserialized_metadata.pkgfs_verity_info().table_params
        );
        assert_eq!(
            original_metadata.pkgfs_type,
            *deserialized_metadata.pkgfs_type(),
        );
    }

    #[test]
    fn test_serialize_non_empty() {
        let metadata = create_test_metadata();
        let serialized = metadata.serialize();

        assert!(serialized.is_ok());
        let serialized = serialized.unwrap();

        assert!(!serialized.is_empty());
        assert!(serialized.len() > 50);
    }

    #[test]
    fn test_fstype_serialization() {
        let mut metadata = create_test_metadata();
        metadata.pkgfs_type = FsType::Erofs;

        let mut serialized =
            std::io::Cursor::new(metadata.serialize().expect("Serialization should succeed"));
        let deserialized =
            PackageMetadata::deserialize(&mut serialized).expect("Deserialization should succeed");

        assert!(matches!(deserialized.pkgfs_type(), FsType::Erofs));

        metadata.pkgfs_type = FsType::Ext4;

        let mut serialized =
            std::io::Cursor::new(metadata.serialize().expect("Serialization should succeed"));
        let deserialized =
            PackageMetadata::deserialize(&mut serialized).expect("Deserialization should succeed");

        assert!(matches!(deserialized.pkgfs_type(), FsType::Ext4));
    }

    #[test]
    fn test_invalid_data_deserialization() {
        let mut invalid_data = std::io::Cursor::new(vec![1, 2, 3, 4, 5]);
        let result = PackageMetadata::deserialize(&mut invalid_data);

        assert!(result.is_err());
    }

    #[test]
    fn test_version_comparison_basic() {
        let metadata_v1 = create_test_metadata_with_version("1.0.0");
        let metadata_v2 = create_test_metadata_with_version("2.0.0");
        let metadata_v1_1 = create_test_metadata_with_version("1.1.0");

        // Test basic version comparison
        assert!(metadata_v1.version() < metadata_v2.version());
        assert!(metadata_v2.version() > metadata_v1.version());
        assert!(metadata_v1.version() < metadata_v1_1.version());
        assert!(metadata_v1_1.version() > metadata_v1.version());
    }

    #[test]
    fn test_version_comparison_semver() {
        let metadata_1_0_0 = create_test_metadata_with_version("1.0.0");
        let metadata_1_0_1 = create_test_metadata_with_version("1.0.1");
        let metadata_1_1_0 = create_test_metadata_with_version("1.1.0");
        let metadata_2_0_0 = create_test_metadata_with_version("2.0.0");

        // Test patch version comparison
        assert!(metadata_1_0_0.version() < metadata_1_0_1.version());

        // Test minor version comparison
        assert!(metadata_1_0_1.version() < metadata_1_1_0.version());

        // Test major version comparison
        assert!(metadata_1_1_0.version() < metadata_2_0_0.version());

        // Test equality
        let metadata_1_0_0_copy = create_test_metadata_with_version("1.0.0");
        assert_eq!(metadata_1_0_0.version(), metadata_1_0_0_copy.version());
    }

    #[test]
    fn test_invalid_version_creation() {
        let invalid_versions = vec![
            "invalid", "1.0", "1.0.0.0", "", "v1.0.0", "1.0.0-", "1.0.0+",
        ];

        for invalid_version in invalid_versions {
            let package_config = PackageConfigSpec {
                package: crate::config::Package {
                    name: "test_package".to_string(),
                    description: "A test package".to_string(),
                    version: invalid_version.to_string(),
                    autostart: Some(true),
                },
                container: crate::config::Container {
                    storage_limit: Some(1000),
                    data_dirs: Some("/test/path1:/test/path2".to_string()),
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
                    service_type: "notify".to_string(),
                    bus_name: Some("test.bus.name".to_string()),
                    remain_after_exit: Some(false),
                },
            };

            let result = PackageMetadata::new(
                package_config,
                FsType::Erofs,
                make_verity("test_hash_root", 0),
            );
            assert!(result.is_err());
        }
    }

    fn create_test_metadata_with_version(version: &str) -> PackageMetadata {
        let package_config = PackageConfigSpec {
            package: crate::config::Package {
                name: "test_package".to_string(),
                description: "A test package".to_string(),
                version: version.to_string(),
                autostart: Some(true),
            },
            container: crate::config::Container {
                storage_limit: Some(1000),
                data_dirs: Some("/test/path1:/test/path2".to_string()),
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
                service_type: "notify".to_string(),
                bus_name: Some("test.bus.name".to_string()),
                remain_after_exit: Some(false),
            },
        };

        PackageMetadata::new(
            package_config,
            FsType::Erofs,
            make_verity("test_hash_root", 0),
        )
        .unwrap()
    }

    #[test]
    fn test_ssam_pkg_metadata_json_roundtrip() {
        let original = create_test_metadata();
        let json = serde_json::to_string(&original).expect("Serialization should succeed");
        let deserialized: PackageMetadata =
            serde_json::from_str(&json).expect("Deserialization should succeed");
        assert_eq!(original, deserialized);
    }

    #[test]
    fn test_ssam_pkg_metadata_json_roundtrip_erofs() {
        let mut metadata = create_test_metadata();
        metadata.pkgfs_type = FsType::Erofs;
        let json = serde_json::to_string(&metadata).expect("Serialization should succeed");
        let deserialized: PackageMetadata =
            serde_json::from_str(&json).expect("Deserialization should succeed");
        assert_eq!(metadata, deserialized);
    }

    #[test]
    fn test_ssam_pkg_metadata_json_roundtrip_ext4() {
        let mut metadata = create_test_metadata();
        metadata.pkgfs_type = FsType::Ext4;
        let json = serde_json::to_string(&metadata).expect("Serialization should succeed");
        let deserialized: PackageMetadata =
            serde_json::from_str(&json).expect("Deserialization should succeed");
        assert_eq!(metadata, deserialized);
    }
}
