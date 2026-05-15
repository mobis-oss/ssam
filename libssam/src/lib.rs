// Copyright 2025-2026 Hyundai Mobis Co., Ltd.
// SPDX-License-Identifier: Apache-2.0

pub mod json_result;
pub mod ssam_package;
// tonic-generated code; pedantic lints suppressed
#[allow(clippy::pedantic)]
pub mod remocon {
    pub const CONTROL_PORT: u16 = 63737;
    tonic::include_proto!("ssamd.remocon");
}

pub use json_result::{
    InspectLocalPackageResponse, InstallResponse, JsonResult, PackageInfoResponse,
    PackageStatusEntry, StartStopResult, TimelineResponse,
};
pub use ssam_package::PackageParseError;

/// Seccomp policy module providing default security profile for containers.
pub mod seccomp {
    include!(concat!(env!("OUT_DIR"), "/default_seccomp_policy.rs"));
}

/// JSON-RPC response types generated from remocon-responses.schema.json.
/// Provides schema version constant and response payload types.
// typify-generated code; clippy lints suppressed
#[allow(clippy::pedantic, clippy::all)]
pub mod remocon_schema {
    include!(concat!(env!("OUT_DIR"), "/remocon_schema_types.rs"));
}

pub mod config {
    // To see the generated struct while in VSCode,
    // # Ctrl + Shift + P
    // # Choose 'rust_analyzer: Expand macro recursively at caret'
    use proc_macros::toml_file_to_struct;
    toml_file_to_struct!("data/package_config_spec.toml");

    // Whether Fixint or Varint is the difference between legacy and standard
    pub static SSAM_SERIALIZATION_CONFIG: bincode::config::Configuration<
        bincode::config::LittleEndian,
        bincode::config::Fixint,
        bincode::config::NoLimit,
    > = bincode::config::legacy();
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
}
