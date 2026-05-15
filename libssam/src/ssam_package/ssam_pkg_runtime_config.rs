// Copyright 2025-2026 Hyundai Mobis Co., Ltd.
// SPDX-License-Identifier: Apache-2.0

use oci_spec::runtime::Spec as OciRuntimeSpec;
use std::{io::Read, path::Path};

use anyhow::{Context, Result, anyhow};

use super::PackageParseError;

#[derive(Debug, Clone, derive_more::Deref)]
pub struct PackageRuntimeConfig(Box<OciRuntimeSpec>);

impl PackageRuntimeConfig {
    pub(crate) fn from_file(config_file: impl AsRef<Path>) -> Result<Self> {
        let runtime_config = OciRuntimeSpec::load(&config_file).with_context(|| {
            format!(
                "Failed to load runtime spec as OCI Runtime Spec - {}",
                config_file.as_ref().display()
            )
        })?;
        Ok(Self(Box::new(runtime_config)))
    }

    pub(crate) fn serialize(&self) -> Result<Vec<u8>> {
        Ok(serde_json::to_vec(&self.0)?)
    }

    pub(crate) fn deserialize<R: Read>(src: &mut R) -> Result<Self, PackageParseError> {
        let mut buf = Vec::new();
        src.read_to_end(&mut buf)
            .context("Reading runtime config bytes from package")
            .map_err(|source| PackageParseError::Io {
                message: "Failed to read runtime config.".to_string(),
                source,
            })?;
        serde_json::from_slice(&buf)
            .map(|spec| Self(Box::new(spec)))
            .map_err(|e| PackageParseError::ParseFailed {
                source: anyhow!(e).context("Failed to parse runtime config"),
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn create_test_runtime_config() -> &'static str {
        r#"{
            "ociVersion": "1.0.0",
            "process": {
                "terminal": false,
                "user": {
                    "uid": 0,
                    "gid": 0
                },
                "args": ["/bin/sh"],
                "env": ["PATH=/usr/bin:/bin"],
                "cwd": "/"
            },
            "root": {
                "path": "rootfs",
                "readonly": true
            },
            "hostname": "test-container",
            "mounts": [],
            "linux": {
                "namespaces": [
                    {
                        "type": "pid"
                    },
                    {
                        "type": "network"
                    }
                ]
            }
        }"#
    }

    #[test]
    fn test_from_file_valid_config() {
        // Create temporary file with valid OCI runtime spec
        let mut temp_file = NamedTempFile::new().expect("Failed to create temp file");
        temp_file
            .write_all(create_test_runtime_config().as_bytes())
            .expect("Failed to write to temp file");

        let result = PackageRuntimeConfig::from_file(temp_file.path());
        assert!(
            result.is_ok(),
            "Should successfully load valid runtime config"
        );

        let config = result.unwrap();
        assert_eq!(config.version(), "1.0.0");
        assert_eq!(config.hostname().as_deref(), Some("test-container"));
    }

    #[test]
    fn test_from_file_invalid_config() {
        // Create temporary file with invalid JSON
        let mut temp_file = NamedTempFile::new().expect("Failed to create temp file");
        temp_file
            .write_all(b"invalid json")
            .expect("Failed to write to temp file");

        let result = PackageRuntimeConfig::from_file(temp_file.path());
        assert!(
            result.is_err(),
            "Should fail to load invalid runtime config"
        );
    }

    #[test]
    fn test_from_file_nonexistent_file() {
        let result = PackageRuntimeConfig::from_file("/nonexistent/path");
        assert!(result.is_err(), "Should fail to load from nonexistent file");
    }

    #[test]
    fn test_serialize_deserialize_roundtrip() {
        // Create a runtime config
        let mut temp_file = NamedTempFile::new().expect("Failed to create temp file");
        temp_file
            .write_all(create_test_runtime_config().as_bytes())
            .expect("Failed to write to temp file");

        let original_config = PackageRuntimeConfig::from_file(temp_file.path())
            .expect("Failed to create runtime config");

        // Serialize
        let serialized = original_config
            .serialize()
            .expect("Failed to serialize runtime config");

        // Deserialize
        let mut cursor = Cursor::new(serialized);
        let deserialized_config = PackageRuntimeConfig::deserialize(&mut cursor)
            .expect("Failed to deserialize runtime config");

        // Compare key fields
        assert_eq!(original_config.version(), deserialized_config.version());
        assert_eq!(original_config.hostname(), deserialized_config.hostname());
    }

    #[test]
    fn test_deserialize_invalid_data() {
        let invalid_data = b"invalid json data";
        let mut cursor = Cursor::new(invalid_data);

        let result = PackageRuntimeConfig::deserialize(&mut cursor);
        assert!(result.is_err(), "Should fail to deserialize invalid data");
    }

    #[test]
    fn test_serialize_success() {
        let mut temp_file = NamedTempFile::new().expect("Failed to create temp file");
        temp_file
            .write_all(create_test_runtime_config().as_bytes())
            .expect("Failed to write to temp file");

        let config = PackageRuntimeConfig::from_file(temp_file.path())
            .expect("Failed to create runtime config");

        let result = config.serialize();
        assert!(
            result.is_ok(),
            "Should successfully serialize runtime config"
        );

        let serialized = result.unwrap();
        assert!(
            !serialized.is_empty(),
            "Serialized data should not be empty"
        );

        // Verify it's valid JSON
        let _: serde_json::Value =
            serde_json::from_slice(&serialized).expect("Serialized data should be valid JSON");
    }

    #[test]
    fn test_deref_functionality() {
        let mut temp_file = NamedTempFile::new().expect("Failed to create temp file");
        temp_file
            .write_all(create_test_runtime_config().as_bytes())
            .expect("Failed to write to temp file");

        let config = PackageRuntimeConfig::from_file(temp_file.path())
            .expect("Failed to create runtime config");

        // Test that we can access OciRuntimeSpec methods through Deref
        assert_eq!(config.version(), "1.0.0");
        assert!(config.process().is_some());
        assert!(config.root().is_some());
    }
}
