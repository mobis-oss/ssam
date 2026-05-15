// Copyright 2025-2026 Hyundai Mobis Co., Ltd.
// SPDX-License-Identifier: Apache-2.0

use anyhow::{Context, Result, anyhow};
use std::io::Read;
use std::path::Path;

use super::PackageParseError;

/// Represents seccomp policy for container security filtering.
/// Stores raw JSON bytes of the seccomp configuration.
#[derive(Debug, Clone)]
pub struct PackageSeccompPolicy(Vec<u8>);

impl PackageSeccompPolicy {
    /// Creates a new seccomp policy from a file path.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be read or its contents are not
    /// valid JSON.
    pub fn from_file(seccomp_file: impl AsRef<Path>) -> Result<Self> {
        let content = std::fs::read(&seccomp_file).with_context(|| {
            format!(
                "Failed to read seccomp policy file: {}",
                seccomp_file.as_ref().display()
            )
        })?;
        // Validate JSON format
        serde_json::from_slice::<serde_json::Value>(&content).with_context(|| {
            format!(
                "Invalid JSON in seccomp policy file: {}",
                seccomp_file.as_ref().display()
            )
        })?;
        Ok(Self(content))
    }

    /// Creates a new seccomp policy from raw bytes.
    /// Validates that the bytes are valid JSON.
    ///
    /// # Errors
    ///
    /// Returns an error if `data` is not valid JSON.
    pub fn from_bytes(data: Vec<u8>) -> Result<Self> {
        // Validate JSON format only; the parsed result is discarded to preserve original bytes
        serde_json::from_slice::<serde_json::Value>(&data)
            .context("Invalid JSON in seccomp policy data")?;
        Ok(Self(data))
    }

    /// Creates a new seccomp policy using the default policy from libssam.
    #[must_use]
    pub fn default_policy() -> Self {
        Self(crate::seccomp::DEFAULT_SECCOMP_POLICY.as_bytes().to_vec())
    }

    /// Serializes the seccomp policy to bytes for storage.
    pub(crate) fn serialize(&self) -> Vec<u8> {
        self.0.clone()
    }

    /// Deserializes seccomp policy from a reader.
    pub(crate) fn deserialize<R: Read>(src: &mut R) -> Result<Self, PackageParseError> {
        let mut buf = Vec::new();
        src.read_to_end(&mut buf)
            .context("Reading seccomp policy bytes from package")
            .map_err(|source| PackageParseError::Io {
                message: "Failed to read seccomp policy.".to_string(),
                source,
            })?;
        serde_json::from_slice::<serde_json::Value>(&buf).map_err(|e| {
            PackageParseError::ParseFailed {
                source: anyhow!(e).context("Failed to parse seccomp policy"),
            }
        })?;
        Ok(Self(buf))
    }

    /// Returns the seccomp policy as a string slice.
    ///
    /// # Errors
    ///
    /// Returns an error if the seccomp policy bytes are not valid UTF-8.
    pub fn as_str(&self) -> Result<&str> {
        std::str::from_utf8(&self.0).context("Seccomp policy contains invalid UTF-8")
    }

    /// Returns the seccomp policy as raw bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn create_test_seccomp_policy() -> &'static str {
        r#"{
            "defaultAction": "SCMP_ACT_ERRNO",
            "architectures": ["SCMP_ARCH_X86_64"],
            "syscalls": [
                {
                    "names": ["read", "write"],
                    "action": "SCMP_ACT_ALLOW"
                }
            ]
        }"#
    }

    #[test]
    fn test_from_file_valid() {
        let mut temp_file = NamedTempFile::new().expect("Failed to create temp file");
        temp_file
            .write_all(create_test_seccomp_policy().as_bytes())
            .expect("Failed to write to temp file");

        let result = PackageSeccompPolicy::from_file(temp_file.path());
        assert!(
            result.is_ok(),
            "Should successfully load valid seccomp policy"
        );

        let policy = result.unwrap();
        assert!(policy.as_str().is_ok());
        assert!(policy.as_str().unwrap().contains("SCMP_ACT_ERRNO"));
    }

    #[test]
    fn test_from_file_invalid_json() {
        let mut temp_file = NamedTempFile::new().expect("Failed to create temp file");
        temp_file
            .write_all(b"invalid json")
            .expect("Failed to write to temp file");

        let result = PackageSeccompPolicy::from_file(temp_file.path());
        assert!(result.is_err(), "Should fail to load invalid JSON");
    }

    #[test]
    fn test_from_file_nonexistent() {
        let result = PackageSeccompPolicy::from_file("/nonexistent/path");
        assert!(result.is_err(), "Should fail to load from nonexistent file");
    }

    #[test]
    fn test_from_bytes_valid() {
        let data = create_test_seccomp_policy().as_bytes().to_vec();
        let result = PackageSeccompPolicy::from_bytes(data);
        assert!(
            result.is_ok(),
            "Should successfully create from valid bytes"
        );
    }

    #[test]
    fn test_from_bytes_invalid() {
        let result = PackageSeccompPolicy::from_bytes(b"not json".to_vec());
        assert!(result.is_err(), "Should fail to create from invalid bytes");
    }

    #[test]
    fn test_default_policy() {
        let policy = PackageSeccompPolicy::default_policy();
        assert!(policy.as_str().is_ok());
        let policy_str = policy.as_str().unwrap();
        // Verify it contains expected seccomp fields
        assert!(policy_str.contains("defaultAction"));
        assert!(policy_str.contains("syscalls"));
    }

    #[test]
    fn test_serialize_deserialize_roundtrip() {
        let original =
            PackageSeccompPolicy::from_bytes(create_test_seccomp_policy().as_bytes().to_vec())
                .expect("Failed to create policy");

        let serialized = original.serialize();

        let mut cursor = Cursor::new(serialized);
        let deserialized =
            PackageSeccompPolicy::deserialize(&mut cursor).expect("Failed to deserialize");

        assert_eq!(original.as_bytes(), deserialized.as_bytes());
    }

    #[test]
    fn test_as_bytes() {
        let data = create_test_seccomp_policy().as_bytes().to_vec();
        let policy = PackageSeccompPolicy::from_bytes(data.clone()).unwrap();
        assert_eq!(policy.as_bytes(), data.as_slice());
    }
}
