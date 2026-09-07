// Copyright 2025-2026 Hyundai Mobis Co., Ltd.
// SPDX-License-Identifier: Apache-2.0

use anyhow::{Context, Result};
use ipnet::Ipv4Net;
use serde::Deserialize;
use std::path::{Path, PathBuf};

/// Per-bridge subnet size used when `[network.bridge] size` is omitted.
pub(crate) const DEFAULT_SUBNET_SIZE: u8 = 29;

/// Address pool base CIDR used when `[network.bridge] base` is omitted.
pub(crate) const DEFAULT_POOL_BASE: &str = "172.20.0.0/16";

const DEFAULT_PACKAGE_EXTENSION: &str = "ssam";
const DEFAULT_PUBLIC_KEY_PATH: &str = "/etc/ssamd/public.key";

/// Get the default config file path (same directory as the ssamd binary).
pub(crate) fn default_config_path() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe_path| exe_path.parent().map(|p| p.join("ssamd.toml")))
        .unwrap_or_else(|| PathBuf::from("./ssamd.toml"))
}

/// Package source, runtime bundle, and package data directories.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageDirConfig {
    /// Optional read-only package source.
    #[serde(default)]
    pub builtin: String,

    /// Optional read-write package source.
    #[serde(default)]
    pub downloaded: String,

    /// Root for versioned OCI bundles.
    #[serde(rename = "oci_bundle")]
    pub oci_bundle_root: String,

    /// Root for package data directories.
    pub data: String,
}

/// Package configuration parsed from `[package]`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageConfig {
    /// Package file extension without a dot.
    #[serde(default = "default_package_extension")]
    pub extension: String,

    /// Cgroup used for package processes.
    #[serde(default)]
    pub cgroup: String,

    /// Public key used for package signature verification.
    #[serde(default = "default_public_key_path")]
    pub public_key: String,

    /// Package directory settings.
    pub dir: PackageDirConfig,
}

fn default_package_extension() -> String {
    DEFAULT_PACKAGE_EXTENSION.to_owned()
}

fn default_public_key_path() -> String {
    DEFAULT_PUBLIC_KEY_PATH.to_owned()
}

/// Debug-only package runtime settings parsed from `[debug]`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DebugConfig {
    /// Optional package filesystem overlay root.
    #[serde(default)]
    pub pkgfs_overlay_dir: String,

    /// Disable dm-verity package filesystem enforcement.
    #[serde(default)]
    pub disable_pkgfs_integrity: bool,

    /// Disable package signature verification while retaining format checks.
    #[serde(default)]
    pub disable_package_verify: bool,
}

/// Daemon-global network configuration, parsed from the optional `[network]` section.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkConfig {
    /// Per-container bridge networking; `None` when absent.
    #[serde(default)]
    pub bridge: Option<BridgeConfig>,
}

/// Per-container bridge networking config, parsed from `[network.bridge]`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BridgeConfig {
    /// Whether the daemon-managed bridge network is enabled. Defaults to false.
    #[serde(default)]
    pub enabled: bool,

    /// Address pool bridges carve per-container subnets from.
    #[serde(default)]
    pub addr_pool: Option<PoolConfig>,
}

/// Address pool definition, parsed from `[network.bridge.addr_pool]`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PoolConfig {
    /// Pool base CIDR.
    #[serde(default)]
    pub base: Option<String>,

    /// Per-bridge subnet size.
    #[serde(default)]
    pub size: Option<u8>,
}

impl BridgeConfig {
    /// Return the configured or default per-bridge subnet size.
    pub(crate) fn size(&self) -> u8 {
        self.addr_pool
            .as_ref()
            .and_then(|p| p.size)
            .unwrap_or(DEFAULT_SUBNET_SIZE)
    }

    /// Return the configured or default address pool base.
    pub(crate) fn base(&self) -> &str {
        self.addr_pool
            .as_ref()
            .and_then(|p| p.base.as_deref())
            .unwrap_or(DEFAULT_POOL_BASE)
    }

    /// Check that bridge settings are usable.
    pub(crate) fn validate(&self) -> Result<()> {
        let pool: Ipv4Net = self
            .base()
            .parse()
            .with_context(|| format!("Invalid network base {:?}", self.base()))?;
        let size = self.size();
        anyhow::ensure!(
            pool.prefix_len().max(1) <= size,
            "network base {pool} is too small for /{size} bridge subnets"
        );
        anyhow::ensure!(
            size <= 30,
            "network size /{size} leaves no usable host (use /30 or larger)"
        );
        Ok(())
    }
}

/// RPC authorization configuration parsed from `[rpc]`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RpcConfig {
    /// Additional peer effective GIDs allowed to access the RPC server.
    #[serde(default)]
    pub allowed_gids: Vec<u32>,
}

/// Configuration manager that holds the runtime configuration.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Configuration {
    package: PackageConfig,

    /// Optional network section.
    #[serde(default)]
    network: Option<NetworkConfig>,

    /// Debug package settings.
    #[serde(default)]
    debug: DebugConfig,

    /// RPC authorization configuration.
    #[serde(default)]
    rpc: RpcConfig,
}

impl Configuration {
    /// Build a configuration from the current TOML sections.
    #[must_use]
    pub fn new(package: PackageConfig, network: Option<NetworkConfig>, debug: DebugConfig) -> Self {
        Self {
            package,
            network,
            debug,
            rpc: RpcConfig::default(),
        }
    }

    /// Load and validate configuration from a TOML file.
    ///
    /// # Errors
    ///
    /// Returns an error when the file cannot be read, parsed, or fails package
    /// or network validation.
    pub fn load_from_path<P: AsRef<Path>>(config_path: P) -> Result<Self> {
        let config_path = config_path.as_ref();
        let config_content = std::fs::read_to_string(config_path).with_context(|| {
            format!(
                "Failed to read config file: {}. Please ensure the file exists and is readable.",
                config_path.display()
            )
        })?;

        let config: Self = toml::from_str(&config_content).with_context(|| {
            format!(
                "Failed to parse config file: {}. Ensure package settings are present in [package] and [package.dir].",
                config_path.display()
            )
        })?;
        config.validate().with_context(|| {
            format!("Invalid package configuration in {}", config_path.display())
        })?;

        if let Some(bridge) = config.network.as_ref().and_then(|n| n.bridge.as_ref()) {
            bridge
                .validate()
                .context("Invalid [network.bridge] section in config file")?;
        }

        log::info!(
            "Successfully loaded runtime configuration from '{}'",
            config_path.display()
        );
        Ok(config)
    }

    fn validate(&self) -> Result<()> {
        let dirs = &self.package.dir;
        anyhow::ensure!(
            !dirs.oci_bundle_root.trim().is_empty(),
            "package.dir.oci_bundle must not be empty"
        );
        anyhow::ensure!(
            Path::new(&dirs.oci_bundle_root).is_absolute(),
            "package.dir.oci_bundle must be an absolute path"
        );
        anyhow::ensure!(
            !dirs.data.trim().is_empty(),
            "package.dir.data must not be empty"
        );
        anyhow::ensure!(
            !dirs.builtin.trim().is_empty() || !dirs.downloaded.trim().is_empty(),
            "At least one of package.dir.builtin or package.dir.downloaded must be configured"
        );
        anyhow::ensure!(
            self.debug.disable_package_verify || !self.package.public_key.trim().is_empty(),
            "package.public_key must not be empty when package verification is enabled"
        );
        Ok(())
    }

    /// Return builtin package source, empty when not configured.
    pub(crate) fn builtin_packages_dir(&self) -> &str {
        &self.package.dir.builtin
    }

    /// Return downloaded package source, empty when not configured.
    #[must_use]
    pub fn downloaded_packages_dir(&self) -> &str {
        &self.package.dir.downloaded
    }

    /// Return the OCI bundle root.
    #[must_use]
    pub fn oci_bundle_root(&self) -> &str {
        &self.package.dir.oci_bundle_root
    }

    /// Return the package data root.
    #[must_use]
    pub fn packages_data_root(&self) -> &str {
        &self.package.dir.data
    }

    /// Return the optional package filesystem overlay root.
    #[must_use]
    pub fn pkgfs_overlay_dir(&self) -> &str {
        &self.debug.pkgfs_overlay_dir
    }

    /// Return the package public key path.
    #[must_use]
    pub fn public_key_path(&self) -> &str {
        &self.package.public_key
    }

    /// Return the package cgroup.
    #[must_use]
    pub fn packages_cgroup(&self) -> &str {
        &self.package.cgroup
    }

    /// Return the package file extension.
    #[must_use]
    pub fn packages_ext(&self) -> &str {
        &self.package.extension
    }

    /// Return daemon network configuration.
    pub(crate) fn network_config(&self) -> Option<&NetworkConfig> {
        self.network.as_ref()
    }

    /// Return bridge networking configuration.
    pub(crate) fn bridge_config(&self) -> Option<&BridgeConfig> {
        self.network_config()?.bridge.as_ref()
    }

    /// Return whether package filesystem integrity is disabled.
    #[must_use]
    pub fn pkgfs_integrity_disabled(&self) -> bool {
        self.debug.disable_pkgfs_integrity
    }

    /// Return whether package signature verification is disabled.
    #[must_use]
    pub fn package_verify_disabled(&self) -> bool {
        self.debug.disable_package_verify
    }

    /// Get additional RPC peer effective GIDs.
    #[must_use]
    pub fn rpc_allowed_gids(&self) -> &[u32] {
        &self.rpc.allowed_gids
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::Arc;
    use tempfile::NamedTempFile;

    pub(crate) fn test_configuration() -> Arc<Configuration> {
        test_configuration_with_package_dirs(
            Path::new("/var/lib/ssamd/builtin"),
            Path::new("/var/lib/ssamd/downloaded"),
        )
    }

    pub(crate) fn test_configuration_with_package_dirs(
        builtin_dir: &Path,
        downloaded_dir: &Path,
    ) -> Arc<Configuration> {
        Arc::new(Configuration::new(
            PackageConfig {
                extension: DEFAULT_PACKAGE_EXTENSION.to_owned(),
                cgroup: String::new(),
                public_key: "/var/lib/ssamd/keys/test.pub.key".to_owned(),
                dir: PackageDirConfig {
                    builtin: builtin_dir
                        .to_str()
                        .expect("builtin test directory contains non-UTF-8 characters")
                        .to_owned(),
                    downloaded: downloaded_dir
                        .to_str()
                        .expect("downloaded test directory contains non-UTF-8 characters")
                        .to_owned(),
                    oci_bundle_root: "/var/lib/ssamd/oci-bundle".to_owned(),
                    data: "/var/lib/ssamd/data".to_owned(),
                },
            },
            Some(NetworkConfig {
                bridge: Some(BridgeConfig {
                    enabled: false,
                    addr_pool: Some(PoolConfig {
                        base: Some(DEFAULT_POOL_BASE.to_owned()),
                        size: Some(DEFAULT_SUBNET_SIZE),
                    }),
                }),
            }),
            DebugConfig::default(),
        ))
    }

    fn write_config(content: &str) -> NamedTempFile {
        let mut file = NamedTempFile::new().unwrap();
        file.write_all(content.as_bytes()).unwrap();
        file.flush().unwrap();
        file
    }

    #[test]
    fn complete_new_configuration_parses() {
        let file = write_config(
            r#"
            [package]
            extension = "tpkg"
            cgroup = "custom.slice"
            public_key = "/custom/key.pub"

            [package.dir]
            builtin = "/custom/base"
            downloaded = "/custom/packages"
            oci_bundle = "/custom/bundle"
            data = "/custom/data"

            [debug]
            pkgfs_overlay_dir = "/custom/overlay"
            disable_pkgfs_integrity = true
            disable_package_verify = false

            [network.bridge]
            enabled = true

            [rpc]
            allowed_gids = [1000, 1001]
        "#,
        );
        let config = Configuration::load_from_path(file.path()).unwrap();
        assert_eq!(config.builtin_packages_dir(), "/custom/base");
        assert_eq!(config.downloaded_packages_dir(), "/custom/packages");
        assert_eq!(config.oci_bundle_root(), "/custom/bundle");
        assert_eq!(config.packages_data_root(), "/custom/data");
        assert!(config.pkgfs_integrity_disabled());
        assert_eq!(config.rpc_allowed_gids(), &[1000, 1001]);
    }

    #[test]
    fn package_defaults_apply() {
        let file = write_config(
            r#"
            [package.dir]
            builtin = "/custom/base"
            oci_bundle = "/custom/bundle"
            data = "/custom/data"
        "#,
        );
        let config = Configuration::load_from_path(file.path()).unwrap();
        assert_eq!(config.packages_ext(), DEFAULT_PACKAGE_EXTENSION);
        assert!(config.packages_cgroup().is_empty());
        assert_eq!(config.public_key_path(), DEFAULT_PUBLIC_KEY_PATH);
        assert!(!config.pkgfs_integrity_disabled());
        assert!(!config.package_verify_disabled());
    }

    #[test]
    fn builtin_only_and_downloaded_only_parse() {
        for (builtin, downloaded) in [("/custom/base", ""), ("", "/custom/packages")] {
            let file = write_config(&format!(
                "[package.dir]\nbuiltin = {builtin:?}\ndownloaded = {downloaded:?}\noci_bundle = \"/custom/bundle\"\ndata = \"/custom/data\"\n"
            ));
            assert!(Configuration::load_from_path(file.path()).is_ok());
        }
    }

    #[test]
    fn required_package_paths_and_source_are_validated() {
        for config in [
            "[package.dir]\nbuiltin = \"/custom/base\"\ndata = \"/custom/data\"\n",
            "[package.dir]\nbuiltin = \"/custom/base\"\noci_bundle = \"/custom/bundle\"\n",
            "[package.dir]\nbuiltin = \"\"\ndownloaded = \"\"\noci_bundle = \"/custom/bundle\"\ndata = \"/custom/data\"\n",
        ] {
            let file = write_config(config);
            assert!(Configuration::load_from_path(file.path()).is_err());
        }
    }

    #[test]
    fn relative_oci_bundle_path_is_rejected() {
        let file = write_config(
            "[package.dir]\nbuiltin = \"/custom/base\"\noci_bundle = \"relative/bundle\"\ndata = \"/custom/data\"\n",
        );
        assert!(Configuration::load_from_path(file.path()).is_err());
    }

    #[test]
    fn empty_public_key_requires_disabled_verification() {
        let enabled = write_config(
            "[package]\npublic_key = \"\"\n[package.dir]\nbuiltin = \"/custom/base\"\noci_bundle = \"/custom/bundle\"\ndata = \"/custom/data\"\n",
        );
        assert!(Configuration::load_from_path(enabled.path()).is_err());

        let disabled = write_config(
            "[package]\npublic_key = \"\"\n[package.dir]\nbuiltin = \"/custom/base\"\noci_bundle = \"/custom/bundle\"\ndata = \"/custom/data\"\n[debug]\ndisable_package_verify = true\n",
        );
        assert!(Configuration::load_from_path(disabled.path()).is_ok());
    }

    #[test]
    fn old_schema_is_rejected() {
        let file = write_config(
            "[common]\ndownloaded_packages_dir = \"/custom/packages\"\npackages_data_root = \"/custom/data\"\npackages_mnt_root = \"/custom/bundle\"\npublic_key_file_path = \"/custom/key.pub\"\npackages_ext = \"ssam\"\npackages_cgroup = \"\"\npackages_overlayfs_root = \"\"\n",
        );
        assert!(Configuration::load_from_path(file.path()).is_err());
    }

    #[test]
    fn bridge_validation_and_rpc_parsing_remain_available() {
        let file = write_config(
            "[package.dir]\nbuiltin = \"/custom/base\"\noci_bundle = \"/custom/bundle\"\ndata = \"/custom/data\"\n[network.bridge]\nenabled = true\n[network.bridge.addr_pool]\nbase = \"172.20.0.0/16\"\nsize = 29\n[rpc]\nallowed_gids = [1000]\n",
        );
        let config = Configuration::load_from_path(file.path()).unwrap();
        assert!(config.bridge_config().is_some_and(|bridge| bridge.enabled));
        assert_eq!(config.rpc_allowed_gids(), &[1000]);
    }

    #[test]
    fn bridge_enabled_defaults_to_false_with_address_pool() {
        let file = write_config(
            "[package.dir]\nbuiltin = \"/custom/base\"\noci_bundle = \"/custom/bundle\"\ndata = \"/custom/data\"\n[network.bridge.addr_pool]\nbase = \"172.20.0.0/16\"\nsize = 29\n",
        );
        let config = Configuration::load_from_path(file.path()).unwrap();
        assert!(config.bridge_config().is_some_and(|bridge| !bridge.enabled));
    }
}
