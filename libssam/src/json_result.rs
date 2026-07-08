// Copyright 2025-2026 Hyundai Mobis Co., Ltd.
// SPDX-License-Identifier: Apache-2.0

use anyhow::{Context, Result, bail};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::ssam_package::ssam_pkg_info::PackageInfoResult;
use crate::ssam_package::ssam_pkg_metadata::PackageMetadata;

fn internal_serialization_error_json() -> String {
    format!(
        r#"{{"schema_version":"{}","success":false,"reason":"Internal serialization error"}}"#,
        crate::remocon_schema::JSON_RESULT_SCHEMA_VERSION
    )
}

/// Generic JSON result wrapper for RPC responses.
///
/// Serializes to `{"success": true/false, ...}` with optional `data` and `reason` fields.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct JsonResult<T> {
    #[serde(default = "default_schema_version")]
    pub schema_version: String,
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

fn default_schema_version() -> String {
    crate::remocon_schema::JSON_RESULT_SCHEMA_VERSION.to_owned()
}

impl<T> JsonResult<T> {
    fn validate_version(&self) -> Result<()> {
        if self.schema_version != crate::remocon_schema::JSON_RESULT_SCHEMA_VERSION {
            bail!(
                "Unsupported schema version: expected '{}', got '{}'",
                crate::remocon_schema::JSON_RESULT_SCHEMA_VERSION,
                self.schema_version
            );
        }
        Ok(())
    }

    fn check_success(&self) -> Result<()> {
        if !self.success {
            bail!("{}", self.reason.as_deref().unwrap_or("Unknown error"));
        }
        Ok(())
    }
}

impl<T: DeserializeOwned> JsonResult<T> {
    /// Parse a JSON RPC response and extract the result.
    ///
    /// For data-carrying responses (`T` = concrete type), the `data` field
    /// must be present.  For action responses (`T = ()`), the `data` field
    /// is optional and `()` is returned on success.
    ///
    /// # Errors
    ///
    /// Returns an error if the JSON is malformed, the schema version is
    /// unsupported, the response indicates failure, or the `data` field is
    /// missing for a data-carrying response type.
    pub fn parse_response(json: &str) -> Result<T> {
        let result: Self = serde_json::from_str(json).context("Failed to parse JSON response")?;
        result.validate_version()?;
        result.check_success()?;
        match result.data {
            Some(data) => Ok(data),
            _ => serde_json::from_value(serde_json::Value::Null)
                .context("Missing data field in response"),
        }
    }
}

impl<T: Serialize> JsonResult<T> {
    /// Creates a successful result with data and serializes to JSON string.
    /// Falls back to error JSON if serialization fails.
    pub fn success(data: T) -> String {
        let result = JsonResult {
            schema_version: default_schema_version(),
            success: true,
            data: Some(data),
            reason: None,
        };

        serde_json::to_string(&result).unwrap_or_else(|e| {
            log::error!("Failed to serialize JsonResult: {e:?}");
            internal_serialization_error_json()
        })
    }

    /// Creates a successful empty result (no data field).
    #[must_use]
    pub fn success_empty() -> String {
        let result: JsonResult<()> = JsonResult {
            schema_version: default_schema_version(),
            success: true,
            data: None,
            reason: None,
        };

        serde_json::to_string(&result).unwrap_or_else(|e| {
            log::error!("Failed to serialize JsonResult: {e:?}");
            internal_serialization_error_json()
        })
    }

    /// Creates a failure result with reason string.
    pub fn failure(reason: impl Into<String>) -> String {
        let result: JsonResult<()> = JsonResult {
            schema_version: default_schema_version(),
            success: false,
            data: None,
            reason: Some(reason.into()),
        };

        serde_json::to_string(&result).unwrap_or_else(|e| {
            log::error!("Failed to serialize JsonResult: {e:?}");
            internal_serialization_error_json()
        })
    }
}

/// Package status entry for `ListPackagesStatus` response.
///
/// Re-exported from generated schema types to maintain API compatibility.
pub type PackageStatusEntry = crate::remocon_schema::PackageStatusEntry;

/// Timeline response for `GetTimelineInfo` RPC.
///
/// Re-exported from generated schema types as `TimelineData` to maintain API compatibility.
pub type TimelineResponse = crate::remocon_schema::TimelineData;

/// Package info response for `GetPackageInfo` RPC.
pub type PackageInfoResponse = crate::remocon_schema::PackageInfoResponse;

pub type InstallResponse = crate::remocon_schema::InstallResponse;

pub type InspectLocalPackageResponse = crate::remocon_schema::InspectLocalPackageResponse;

pub type StartStopResult = crate::remocon_schema::StartStopResult;

#[must_use]
pub fn serialize_metadata(metadata: &PackageMetadata) -> crate::remocon_schema::PackageInfo {
    crate::remocon_schema::PackageInfo {
        package_name: metadata.get_package_name().to_owned(),
        version: metadata.get_package_version().to_owned(),
        description: metadata.package.description.clone(),
    }
}

impl From<&PackageMetadata> for InstallResponse {
    fn from(metadata: &PackageMetadata) -> Self {
        Self {
            metadata: serialize_metadata(metadata),
        }
    }
}

impl From<PackageInfoResult> for PackageInfoResponse {
    fn from(result: PackageInfoResult) -> Self {
        match result {
            PackageInfoResult::Normal(info) => Self {
                broken: false,
                name: info.package_name,
                status: info.package_status,
                metadata: Some(serialize_metadata(&info.package_metadata)),
                quota: Some(crate::remocon_schema::QuotaInformation {
                    enabled: info.quota_info.enabled,
                    limit: info.quota_info.limit,
                }),
                package_path: info.package_path,
                error_summary: None,
                error_details: None,
            },
            PackageInfoResult::Broken { filename, info } => Self {
                broken: true,
                name: filename,
                status: "Broken".to_string(),
                metadata: None,
                quota: None,
                package_path: info.package_path,
                error_summary: Some(info.broken_info.summary),
                error_details: Some(info.broken_info.details),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_test_metadata(name: &str, version: &str) -> PackageMetadata {
        PackageMetadata::new(
            crate::config::PackageConfigSpec {
                package: crate::config::Package {
                    name: name.to_string(),
                    version: version.to_string(),
                    description: "Test package".to_string(),
                    autostart: Some(true),
                },
                container: crate::config::Container {
                    storage_limit: Some(2048),
                    data_dirs: Some("/app/data".to_string()),
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
                    bus_name: None,
                    remain_after_exit: Some(false),
                },
            },
            crate::superblock::FsType::Erofs,
            crate::ssam_package::PackageFsVerityInfo {
                data_size: 0,
                hash_size: 0,
                table_params: "1 4096 4096 100 101 sha256 dummy_hash_root deadbeef".to_string(),
                hash_offset: 0,
            },
        )
        .expect("test metadata should be valid")
    }

    #[test]
    fn test_json_result_parses_without_schema_version() {
        // Backward-compat: allow parsing JSON produced before schema_version
        // was introduced.
        let json_str = r#"{"success": false, "reason": "old"}"#;
        let parsed: JsonResult<()> = serde_json::from_str(json_str).unwrap();
        assert_eq!(
            parsed.schema_version,
            crate::remocon_schema::JSON_RESULT_SCHEMA_VERSION
        );
        assert!(!parsed.success);
        assert_eq!(parsed.reason.as_deref(), Some("old"));
    }

    #[test]
    fn test_json_result_rejects_unsupported_schema_version() {
        let json_str = r#"{"schema_version":"2","success":false,"reason":"test"}"#;
        let result = JsonResult::<()>::parse_response(json_str);
        assert!(result.is_err());
        let err_msg = format!("{}", result.unwrap_err());
        assert!(err_msg.contains("Unsupported schema version"));
        assert!(err_msg.contains("expected '1'"));
        assert!(err_msg.contains("got '2'"));
    }

    #[test]
    fn test_json_result_accepts_valid_schema_version() {
        let json_str = r#"{"schema_version":"1","success":true,"data":"ok"}"#;
        let result = JsonResult::<String>::parse_response(json_str);
        assert!(result.is_ok());
    }

    #[test]
    fn test_parse_response_action_success() {
        let json_str = r#"{"schema_version":"1","success":true}"#;
        let result = JsonResult::<()>::parse_response(json_str);
        assert!(result.is_ok());
    }

    #[test]
    fn test_parse_response_action_failure_with_reason() {
        let json_str = r#"{"schema_version":"1","success":false,"reason":"Package not found"}"#;
        let result = JsonResult::<()>::parse_response(json_str);
        assert!(result.is_err());
        let err_msg = format!("{}", result.unwrap_err());
        assert!(err_msg.contains("Package not found"));
    }

    #[test]
    fn test_parse_response_action_unsupported_version() {
        let json_str = r#"{"schema_version":"2","success":true}"#;
        let result = JsonResult::<()>::parse_response(json_str);
        assert!(result.is_err());
        let err_msg = format!("{}", result.unwrap_err());
        assert!(err_msg.contains("Unsupported schema version"));
    }

    #[test]
    fn test_parse_response_success() {
        let json_str = r#"{"schema_version":"1","success":true,"data":"hello"}"#;
        let result = JsonResult::<String>::parse_response(json_str);
        assert_eq!(result.unwrap(), "hello");
    }

    #[test]
    fn test_parse_response_failure() {
        let json_str = r#"{"schema_version":"1","success":false,"reason":"not found"}"#;
        let result = JsonResult::<String>::parse_response(json_str);
        assert!(result.is_err());
        let err_msg = format!("{}", result.unwrap_err());
        assert!(err_msg.contains("not found"));
    }

    #[test]
    fn test_parse_response_missing_data() {
        let json_str = r#"{"schema_version":"1","success":true}"#;
        let result = JsonResult::<String>::parse_response(json_str);
        assert!(result.is_err());
        let err_msg = format!("{}", result.unwrap_err());
        assert!(err_msg.contains("Missing data field"));
    }

    #[test]
    fn test_json_result_success_with_data() {
        let data = PackageStatusEntry {
            package_name: "test-pkg".to_string(),
            status: "Ready".to_string(),
        };
        let json_str = JsonResult::success(data);
        let parsed: serde_json::Value = serde_json::from_str(&json_str).unwrap();

        assert_eq!(
            parsed["schema_version"],
            crate::remocon_schema::JSON_RESULT_SCHEMA_VERSION
        );
        assert_eq!(parsed["success"], true);
        assert_eq!(parsed["data"]["package_name"], "test-pkg");
        assert_eq!(parsed["data"]["status"], "Ready");
        assert!(parsed.get("reason").is_none());
    }

    #[test]
    fn test_json_result_success_empty() {
        let json_str = JsonResult::<()>::success_empty();
        let parsed: serde_json::Value = serde_json::from_str(&json_str).unwrap();

        assert_eq!(
            parsed["schema_version"],
            crate::remocon_schema::JSON_RESULT_SCHEMA_VERSION
        );
        assert_eq!(parsed["success"], true);
        assert!(parsed.get("data").is_none());
        assert!(parsed.get("reason").is_none());
    }

    #[test]
    fn test_json_result_failure() {
        let json_str = JsonResult::<()>::failure("Package not found: my-pkg");
        let parsed: serde_json::Value = serde_json::from_str(&json_str).unwrap();

        assert_eq!(
            parsed["schema_version"],
            crate::remocon_schema::JSON_RESULT_SCHEMA_VERSION
        );
        assert_eq!(parsed["success"], false);
        assert_eq!(parsed["reason"], "Package not found: my-pkg");
        assert!(parsed.get("data").is_none());
    }

    #[test]
    fn test_package_status_entry_round_trip() {
        let entry = PackageStatusEntry {
            package_name: "MyPackage".to_string(),
            status: "Running".to_string(),
        };

        let json_str = serde_json::to_string(&entry).unwrap();
        let deserialized: PackageStatusEntry = serde_json::from_str(&json_str).unwrap();

        assert_eq!(entry.package_name, deserialized.package_name);
        assert_eq!(entry.status, deserialized.status);
    }

    #[test]
    fn test_install_response_round_trip() {
        let metadata = build_test_metadata("my-package", "1.2.3");
        let response = InstallResponse::from(&metadata);

        let json = serde_json::to_string(&response).unwrap();
        let restored: InstallResponse = serde_json::from_str(&json).unwrap();

        assert_eq!(restored, response);
    }

    #[test]
    fn test_parse_install_response_success() {
        let metadata = build_test_metadata("pkg-a", "2.0.0");
        let json = JsonResult::success(InstallResponse::from(&metadata));

        let parsed = JsonResult::<InstallResponse>::parse_response(&json).unwrap();

        assert_eq!(parsed.metadata.package_name, "pkg-a");
        assert_eq!(parsed.metadata.version, "2.0.0");
    }

    #[test]
    fn test_parse_install_response_missing_metadata() {
        let json = r#"{"schema_version":"1","success":true,"data":{}}"#;
        let result = JsonResult::<InstallResponse>::parse_response(json);

        assert!(result.is_err());
        assert!(format!("{}", result.unwrap_err()).contains("Failed to parse JSON response"));
    }

    #[test]
    fn test_parse_install_response_failure() {
        let json = r#"{"schema_version":"1","success":false,"reason":"install failed"}"#;
        let result = JsonResult::<InstallResponse>::parse_response(json);

        assert!(result.is_err());
        assert!(format!("{}", result.unwrap_err()).contains("install failed"));
    }

    #[test]
    fn test_package_info_response_normal_variant() {
        let response: PackageInfoResponse =
            PackageInfoResult::Normal(crate::ssam_package::ssam_pkg_info::PackageInfo {
                package_name: "test-pkg".to_string(),
                package_path: "/packages/test-pkg.ssam".to_string(),
                package_metadata: build_test_metadata("test-pkg", "1.0.0"),
                package_status: "Running".to_string(),
                quota_info: crate::ssam_package::ssam_pkg_info::QuotaInformation {
                    enabled: true,
                    limit: 2048,
                },
            })
            .into();

        assert!(!response.broken);
        assert_eq!(response.name, "test-pkg");
        assert_eq!(response.status, "Running");
        assert!(response.metadata.is_some());
        assert!(response.quota.is_some());
        assert_eq!(response.package_path, "/packages/test-pkg.ssam");
        assert!(response.error_summary.is_none());
        assert!(response.error_details.is_none());

        let json_str = serde_json::to_string(&response).unwrap();
        let json_value: serde_json::Value = serde_json::from_str(&json_str).unwrap();
        assert_eq!(json_value["package_path"], "/packages/test-pkg.ssam");
        let deserialized: PackageInfoResponse = serde_json::from_str(&json_str).unwrap();

        assert_eq!(response, deserialized);
    }

    #[test]
    fn test_package_info_response_broken_variant() {
        let broken_info = crate::ssam_package::ssam_pkg_info::BrokenReason::new(
            "Parse error",
            "Failed to parse package.toml",
        );
        let result = PackageInfoResult::Broken {
            filename: "broken-pkg".to_string(),
            info: crate::ssam_package::ssam_pkg_info::BrokenPackageInfo {
                package_path: "/packages/broken-pkg.ssam".to_string(),
                broken_info,
            },
        };
        let response: PackageInfoResponse = result.into();

        assert!(response.broken);
        assert_eq!(response.name, "broken-pkg");
        assert_eq!(response.status, "Broken");
        assert!(response.metadata.is_none());
        assert!(response.quota.is_none());
        assert_eq!(response.package_path, "/packages/broken-pkg.ssam");
        assert!(response.error_summary.is_some());
        assert!(response.error_details.is_some());

        let json_str = r#"{"broken":true,"name":"broken-pkg","status":"Broken","package_path":"/packages/broken-pkg.ssam","error_summary":"Parse error","error_details":"Failed to parse package.toml"}"#;
        let deserialized: PackageInfoResponse = serde_json::from_str(json_str).unwrap();

        assert_eq!(deserialized.package_path, "/packages/broken-pkg.ssam");
        assert_eq!(deserialized, response);
    }

    #[test]
    fn test_list_packages_status_response() {
        let entries = vec![
            PackageStatusEntry {
                package_name: "Package1".to_string(),
                status: "Ready".to_string(),
            },
            PackageStatusEntry {
                package_name: "Package2".to_string(),
                status: "Running".to_string(),
            },
        ];

        let json_str = JsonResult::success(entries.clone());
        let parsed: serde_json::Value = serde_json::from_str(&json_str).unwrap();

        assert_eq!(parsed["success"], true);
        assert!(parsed["data"].is_array());
        assert_eq!(parsed["data"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn test_json_result_serialization_failure_fallback() {
        use serde::{Serialize, Serializer};

        #[derive(Debug)]
        struct NonSerializable;

        impl Serialize for NonSerializable {
            fn serialize<S>(&self, _serializer: S) -> Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                // Deliberately fail during serialization
                use serde::ser::Error;
                Err(S::Error::custom("intentional serialization failure"))
            }
        }

        let json_str = JsonResult::success(NonSerializable);

        // Should fall back to INTERNAL_SERIALIZATION_ERROR_JSON
        let parsed: serde_json::Value = serde_json::from_str(&json_str).unwrap();
        assert_eq!(
            parsed["schema_version"],
            crate::remocon_schema::JSON_RESULT_SCHEMA_VERSION
        );
        assert_eq!(parsed["success"], false);
        assert_eq!(parsed["reason"], "Internal serialization error");
        assert!(parsed.get("data").is_none());
    }

    #[test]
    fn test_json_result_success_empty_serialization_cannot_fail() {
        // success_empty uses JsonResult<()>, which should never fail serialization
        let json_str = JsonResult::<()>::success_empty();
        let parsed: serde_json::Value = serde_json::from_str(&json_str).unwrap();
        assert_eq!(parsed["success"], true);
    }

    #[test]
    fn test_json_result_failure_serialization_cannot_fail() {
        // failure uses JsonResult<()>, which should never fail serialization
        let json_str = JsonResult::<()>::failure("test error");
        let parsed: serde_json::Value = serde_json::from_str(&json_str).unwrap();
        assert_eq!(parsed["success"], false);
        assert_eq!(parsed["reason"], "test error");
    }

    #[test]
    fn test_schema_version_matches_generated_constant() {
        // Verify centralized schemaVersion definition matches the generated constant.
        // This prevents drift between schema file and build.rs generation.
        let schema_str = include_str!("../data/remocon-responses.schema.json");
        let schema: serde_json::Value =
            serde_json::from_str(schema_str).expect("Schema file should be valid JSON");

        let schema_version = schema["$defs"]["schemaVersion"]["const"]
            .as_str()
            .expect("schemaVersion definition should have const value");

        assert_eq!(
            schema_version,
            crate::remocon_schema::JSON_RESULT_SCHEMA_VERSION,
            "schemaVersion const should match generated constant"
        );
    }

    #[test]
    fn test_timeline_event_json_round_trip() {
        use crate::remocon_schema::{TimelineEvent, TimelineEventKind};

        let event = TimelineEvent {
            pkg: "my-pkg".to_string(),
            phase: "mount".to_string(),
            duration_ns: 123_456_789,
            kind: TimelineEventKind::Started,
        };

        let json = serde_json::to_string(&event).expect("serialization should succeed");
        let restored: TimelineEvent =
            serde_json::from_str(&json).expect("deserialization should succeed");

        assert_eq!(restored.pkg, event.pkg);
        assert_eq!(restored.phase, event.phase);
        assert_eq!(restored.duration_ns, event.duration_ns);
        assert_eq!(restored.kind, event.kind);
    }

    #[test]
    fn test_timeline_event_completed_kind_round_trip() {
        use crate::remocon_schema::{TimelineEvent, TimelineEventKind};

        let event = TimelineEvent {
            pkg: "svc".to_string(),
            phase: "setup".to_string(),
            duration_ns: 0,
            kind: TimelineEventKind::Completed,
        };

        let json = serde_json::to_string(&event).expect("serialization should succeed");
        let restored: TimelineEvent =
            serde_json::from_str(&json).expect("deserialization should succeed");

        assert_eq!(restored.kind, TimelineEventKind::Completed);
    }

    #[test]
    fn test_timeline_data_json_round_trip() {
        use std::time::Duration;

        use crate::remocon_schema::{TimelineEvent, TimelineEventKind};

        let data = TimelineResponse {
            events: vec![
                TimelineEvent {
                    pkg: "pkg-a".to_string(),
                    phase: "mount".to_string(),
                    duration_ns: 100,
                    kind: TimelineEventKind::Started,
                },
                TimelineEvent {
                    pkg: "pkg-a".to_string(),
                    phase: "mount".to_string(),
                    duration_ns: 200,
                    kind: TimelineEventKind::Completed,
                },
            ],
            ssamd_uptime: Duration::from_secs(42),
        };

        let json_str = JsonResult::success(data);
        let parsed: serde_json::Value =
            serde_json::from_str(&json_str).expect("should be valid JSON");

        assert_eq!(parsed["success"], true);
        let events = parsed["data"]["events"]
            .as_array()
            .expect("events should be array");
        assert_eq!(events.len(), 2);
        assert_eq!(events[0]["pkg"], "pkg-a");
        assert_eq!(events[0]["kind"], "started");
        assert_eq!(events[1]["kind"], "completed");
    }

    #[test]
    fn test_serialize_metadata_maps_essential_fields() {
        let metadata = build_test_metadata("my-app", "3.2.1");
        let serialized = serialize_metadata(&metadata);

        assert_eq!(serialized.package_name, "my-app");
        assert_eq!(serialized.version, "3.2.1");
        assert_eq!(serialized.description, "Test package");
    }

    #[test]
    fn test_metadata_json_shape_all_fields() {
        let metadata = build_test_metadata("test-pkg", "1.0.0");
        let json = serde_json::to_value(&metadata).expect("metadata serialization should succeed");
        let pretty = serde_json::to_string_pretty(&json).expect("pretty printing should succeed");
        eprintln!("=== PackageMetadata JSON shape (all fields) ===\n{pretty}\n===");
        // Verify top-level fields exist
        assert!(
            json.get("build_date").is_some(),
            "build_date field should exist"
        );
        assert!(
            json.get("pkgfs_type").is_some(),
            "pkgfs_type field should exist"
        );
        assert!(
            json.get("pkgfs_verity_info").is_some(),
            "pkgfs_verity_info field should exist"
        );
        assert!(
            json.get("package_config").is_some(),
            "package_config field should exist"
        );
    }

    #[test]
    fn test_metadata_json_shape_optional_none() {
        // Build metadata with optional fields set to None
        let metadata = PackageMetadata::new(
            crate::config::PackageConfigSpec {
                package: crate::config::Package {
                    name: "minimal-pkg".to_string(),
                    version: "1.0.0".to_string(),
                    description: "Minimal test package".to_string(),
                    autostart: None, // Optional field - None
                },
                container: crate::config::Container {
                    storage_limit: None, // Optional field - None
                    data_dirs: None,     // Optional field - None
                    security: crate::config::Security {
                        seccomp: false,
                        mac: false,
                    },
                    network: Some(crate::config::Network {
                        mode: None,
                        bridge: None,
                    }),
                },
                service: crate::config::Service {
                    service_type: "simple".to_string(),
                    bus_name: None,          // Optional field - None
                    remain_after_exit: None, // Optional field - None
                },
            },
            crate::superblock::FsType::Ext4,
            crate::ssam_package::PackageFsVerityInfo {
                data_size: 1024,
                hash_size: 512,
                table_params: "1 4096 4096 100 101 sha256 abc123def456 deadbeef".to_string(),
                hash_offset: 4096,
            },
        )
        .expect("test metadata should be valid");

        let json = serde_json::to_value(&metadata).expect("metadata serialization should succeed");
        // Verify None fields are serialized as null (default serde behavior)
        let package_config = json
            .get("package_config")
            .expect("package_config should exist");
        let container = package_config
            .get("container")
            .expect("container should exist");
        assert_eq!(
            container.get("storage_limit"),
            Some(&serde_json::Value::Null),
            "None storage_limit should be serialized as null"
        );
        assert_eq!(
            container.get("data_dirs"),
            Some(&serde_json::Value::Null),
            "None data_dirs should be serialized as null"
        );
        let service = package_config.get("service").expect("service should exist");
        assert_eq!(
            service.get("bus_name"),
            Some(&serde_json::Value::Null),
            "None bus_name should be serialized as null"
        );
        assert_eq!(
            service.get("remain_after_exit"),
            Some(&serde_json::Value::Null),
            "None remain_after_exit should be serialized as null"
        );
    }
}
