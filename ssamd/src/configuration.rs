// Copyright 2025-2026 Hyundai Mobis Co., Ltd.
// SPDX-License-Identifier: Apache-2.0

use anyhow::{Context, Result};
use ipnet::Ipv4Net;
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::OnceLock;

/// Per-bridge subnet size used when `[network.bridge] size` is omitted.
pub(crate) const DEFAULT_SUBNET_SIZE: u8 = 29;

/// Address pool base CIDR used when `[network.bridge] base` is omitted — RFC1918 private space.
pub(crate) const DEFAULT_POOL_BASE: &str = "172.20.0.0/16";

/// Get the default config file path (same directory as the ssamd binary)
#[cfg(not(test))]
pub(crate) fn default_config_path() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe_path| exe_path.parent().map(|p| p.join("ssamd.toml")))
        .unwrap_or_else(|| PathBuf::from("./ssamd.toml"))
}

#[cfg(test)]
pub(crate) fn default_config_path() -> PathBuf {
    // Test environment: Use testdata/miscs/ssamd.toml relative to workspace root
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest_dir.join("testdata/testconfig/ssamd.toml")
}

/// Common runtime configuration structure that matches build-time config values
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Common {
    /// Bundled (read-only) package directory path (required, must not be empty)
    pub(crate) bundled_packages_dir: String,

    /// Downloaded (read-write) package directory path (required, must not be empty)
    pub(crate) downloaded_packages_dir: String,

    /// Package data root directory (required)
    pub(crate) packages_data_root: String,

    /// Package overlay filesystem root (required)
    pub(crate) packages_overlayfs_root: String,

    /// Package mount root directory (required)
    pub(crate) packages_mnt_root: String,

    /// Public key file path (required)
    pub(crate) public_key_file_path: String,

    /// Package cgroup (required)
    pub(crate) packages_cgroup: String,

    /// Package file extension (required)
    pub(crate) packages_ext: String,

    /// Remote control server bind IP (optional)
    pub(crate) rpc_bind_ip: Option<String>,
}

/// Daemon-global network configuration, parsed from the optional `[network]` TOML section
#[derive(Debug, Clone, Deserialize)]
pub struct NetworkConfig {
    /// Per-container bridge networking (`[network.bridge]`); `None` when absent.
    pub bridge: Option<BridgeConfig>,
}

/// Per-container bridge networking config, parsed from `[network.bridge]`.
#[derive(Debug, Clone, Deserialize)]
pub struct BridgeConfig {
    /// Whether the daemon-managed bridge network is enabled. Gates only bridge
    /// networking; host/none container network modes work regardless.
    pub enabled: bool,

    /// Address pool bridges carve per-container subnets from
    /// (`[network.bridge.addr_pool]`); `None` → built-in default.
    pub addr_pool: Option<PoolConfig>,
}

/// Address pool definition, parsed from `[network.bridge.addr_pool]`.
#[derive(Debug, Clone, Deserialize)]
pub struct PoolConfig {
    /// Pool base CIDR (e.g. "172.20.0.0/16"); `None` → default (172.20.0.0/16).
    pub base: Option<String>,

    /// Per-bridge subnet size (prefix length); `None` → default (29).
    pub size: Option<u8>,
}

impl BridgeConfig {
    /// The per-bridge subnet size, from the pool or the default.
    pub(crate) fn size(&self) -> u8 {
        self.addr_pool
            .as_ref()
            .and_then(|p| p.size)
            .unwrap_or(DEFAULT_SUBNET_SIZE)
    }

    /// The address pool base CIDR, from the pool or the default.
    pub(crate) fn base(&self) -> &str {
        self.addr_pool
            .as_ref()
            .and_then(|p| p.base.as_deref())
            .unwrap_or(DEFAULT_POOL_BASE)
    }

    /// Check the bridge settings are usable, so a bad `[network.bridge]` section
    /// fails when the config loads instead of at the first container attach.
    ///
    /// # Errors
    ///
    /// Fails if `base` is not a valid IPv4 CIDR, the pool is too small to hold one
    /// `/size` subnet, or `size` is longer than /30 (e.g. /31 or /32), which
    /// leaves no usable host address.
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

/// Runtime configuration structure for TOML file
#[derive(Debug, Clone, Deserialize)]
struct RuntimeConfig {
    common: Common,

    /// Optional network section — `None` when `[network]` is absent from the config file
    #[serde(default)]
    network: Option<NetworkConfig>,
}

/// Configuration manager that holds the runtime configuration
#[derive(Debug, Clone)]
pub(crate) struct Configuration {
    runtime_config: RuntimeConfig,
}

impl Configuration {
    /// Load configuration from a file path
    fn load_from_path<P: AsRef<std::path::Path>>(config_path: P) -> Result<Self> {
        let config_path = config_path.as_ref();

        let config_content = std::fs::read_to_string(config_path).with_context(|| {
            format!(
                "Failed to read config file: {}. Please ensure the file exists and is readable.",
                config_path.display()
            )
        })?;

        // All required fields must be present in TOML, otherwise it will fail
        let runtime_config: RuntimeConfig = toml::from_str(&config_content).with_context(|| {
            format!(
                "Failed to parse config file: {}. Ensure all required fields are present in [common] section.",
                config_path.display()
            )
        })?;

        if let Some(bridge) = runtime_config
            .network
            .as_ref()
            .and_then(|n| n.bridge.as_ref())
        {
            bridge
                .validate()
                .context("Invalid [network.bridge] section in config file")?;
        }

        log::info!(
            "Successfully loaded runtime configuration from '{}'",
            config_path.display()
        );

        Ok(Self { runtime_config })
    }

    /// Get bundled packages directory path as a borrowed reference
    pub(crate) fn bundled_packages_dir(&self) -> &str {
        &self.runtime_config.common.bundled_packages_dir
    }

    /// Get downloaded packages directory path as a borrowed reference
    pub(crate) fn downloaded_packages_dir(&self) -> &str {
        &self.runtime_config.common.downloaded_packages_dir
    }

    /// Get packages data root directory path as a borrowed reference
    pub(crate) fn packages_data_root(&self) -> &str {
        &self.runtime_config.common.packages_data_root
    }

    /// Get packages overlayfs root directory path as a borrowed reference
    pub(crate) fn packages_overlayfs_root(&self) -> &str {
        &self.runtime_config.common.packages_overlayfs_root
    }

    /// Get packages mount root directory path as a borrowed reference
    pub(crate) fn packages_mnt_root(&self) -> &str {
        &self.runtime_config.common.packages_mnt_root
    }

    /// Get public key file path as a borrowed reference
    pub(crate) fn public_key_file_path(&self) -> &str {
        &self.runtime_config.common.public_key_file_path
    }

    /// Get packages cgroup as a borrowed reference
    pub(crate) fn packages_cgroup(&self) -> &str {
        &self.runtime_config.common.packages_cgroup
    }

    /// Get packages file extension as a borrowed reference
    pub(crate) fn packages_ext(&self) -> &str {
        &self.runtime_config.common.packages_ext
    }

    /// Get bind IP address as a borrowed reference (optional)
    pub(crate) fn rpc_bind_ip(&self) -> Option<&str> {
        self.runtime_config.common.rpc_bind_ip.as_deref()
    }

    /// Get daemon network configuration (`None` when `[network]` section is absent)
    pub(crate) fn network_config(&self) -> Option<&NetworkConfig> {
        self.runtime_config.network.as_ref()
    }

    /// Get bridge networking config (`None` when `[network.bridge]` is absent).
    pub(crate) fn bridge_config(&self) -> Option<&BridgeConfig> {
        self.network_config()?.bridge.as_ref()
    }
}

/// Global static configuration storage (initialized once at startup)
static RUNTIME_CONFIG: OnceLock<Configuration> = OnceLock::new();

/// Initialize the configuration system
///
/// This function initializes the configuration by loading it from the specified path.
///
/// # Arguments
///
/// * `config_path` - Path to the configuration file.
///
/// # Panics
///
/// Panics if:
/// - The configuration has already been initialized
/// - The configuration file cannot be loaded
/// - The configuration file is missing required fields
///
/// # Examples
///
/// ```ignore
/// // Initialize with custom path
/// init(std::path::Path::new("/custom/path/ssamd.toml"));
/// ```
pub fn init<P: AsRef<std::path::Path>>(config_path: P) {
    let path = config_path.as_ref().to_path_buf();

    let config = Configuration::load_from_path(&path).unwrap_or_else(|e| {
        log::error!("Failed to load required configuration: {e}");
        panic!(
            "Configuration file is required. Please ensure {} exists with all required fields.",
            path.display()
        );
    });

    RUNTIME_CONFIG.set(config).unwrap_or_else(|_| {
        panic!("Configuration has already been initialized");
    });
}

/// Get a reference to the bundled packages directory path with static lifetime
///
/// # Panics
///
/// Panics if `init()` has not been called before this function.
pub fn bundled_packages_dir() -> &'static str {
    RUNTIME_CONFIG
        .get()
        .expect("Configuration not initialized. Call init() first.")
        .bundled_packages_dir()
}

/// Get a reference to the downloaded packages directory path with static lifetime
///
/// # Panics
///
/// Panics if `init()` has not been called before this function.
pub fn downloaded_packages_dir() -> &'static str {
    RUNTIME_CONFIG
        .get()
        .expect("Configuration not initialized. Call init() first.")
        .downloaded_packages_dir()
}

/// Get a reference to the packages data root directory path with static lifetime
///
/// Returns `&'static str` pointing to either the runtime config value or build-time config.
///
/// # Panics
///
/// Panics if the daemon configuration has not been initialized via [`init`].
pub fn packages_data_root() -> &'static str {
    RUNTIME_CONFIG
        .get()
        .expect("Configuration not initialized. Call init() first.")
        .packages_data_root()
}

/// Get a reference to the packages overlayfs root directory path with static lifetime
///
/// Returns `&'static str` pointing to either the runtime config value or build-time config.
pub(crate) fn packages_overlayfs_root() -> &'static str {
    RUNTIME_CONFIG
        .get()
        .expect("Configuration not initialized. Call init() first.")
        .packages_overlayfs_root()
}

/// Get a reference to the packages mount root directory path with static lifetime
///
/// Returns `&'static str` pointing to either the runtime config value or build-time config.
///
/// # Panics
///
/// Panics if the daemon configuration has not been initialized via [`init`].
pub fn packages_mnt_root() -> &'static str {
    RUNTIME_CONFIG
        .get()
        .expect("Configuration not initialized. Call init() first.")
        .packages_mnt_root()
}

/// Get a reference to the public key file path with static lifetime
///
/// Returns `&'static str` pointing to either the runtime config value or build-time config.
pub(crate) fn public_key_file_path() -> &'static str {
    RUNTIME_CONFIG
        .get()
        .expect("Configuration not initialized. Call init() first.")
        .public_key_file_path()
}

/// Get a reference to the packages cgroup with static lifetime
///
/// Returns `&'static str` pointing to either the runtime config value or build-time config.
pub(crate) fn packages_cgroup() -> &'static str {
    RUNTIME_CONFIG
        .get()
        .expect("Configuration not initialized. Call init() first.")
        .packages_cgroup()
}

/// Get a reference to the packages file extension with static lifetime
///
/// Returns `&'static str` pointing to either the runtime config value or build-time config.
///
/// # Panics
///
/// Panics if `init()` has not been called before this function.
pub fn packages_ext() -> &'static str {
    RUNTIME_CONFIG
        .get()
        .expect("Configuration not initialized. Call init() first.")
        .packages_ext()
}

/// Get a reference to the bind IP address with static lifetime (optional)
pub(crate) fn rpc_bind_ip() -> Option<&'static str> {
    RUNTIME_CONFIG
        .get()
        .expect("Configuration not initialized. Call init() first.")
        .rpc_bind_ip()
}

/// Get the bridge networking config (`None` when `[network.bridge]` is absent)
///
/// # Panics
///
/// Panics if `init()` has not been called before this function.
pub(crate) fn bridge_config() -> Option<&'static BridgeConfig> {
    RUNTIME_CONFIG
        .get()
        .expect("Configuration not initialized. Call init() first.")
        .bridge_config()
}

/// Ensures test configuration is initialized. Safe to call multiple times.
/// This function is only available in test builds.
#[cfg(test)]
pub(crate) fn ensure_test_init() {
    use std::sync::OnceLock;
    static TEST_INIT: OnceLock<()> = OnceLock::new();
    TEST_INIT.get_or_init(|| {
        let config_path = default_config_path();
        init(config_path);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    /// Helper function to create a complete test configuration file
    fn create_test_config_file() -> NamedTempFile {
        let mut temp_file = NamedTempFile::new().unwrap();
        let config_content = r#"
            [common]
            bundled_packages_dir = "/var/lib/ssamd/bundled"
            downloaded_packages_dir = "/test/packages"
            packages_data_root = "/test/data"
            packages_overlayfs_root = "/test/overlay"
            packages_mnt_root = "/test/mnt"
            public_key_file_path = "/test/key.pub"
            packages_cgroup = "test.slice"
            packages_ext = "tpkg"
        "#;
        temp_file.write_all(config_content.as_bytes()).unwrap();
        temp_file.flush().unwrap();
        temp_file
    }

    #[test]
    fn test_no_config_file() {
        // Should return error when TOML file does not exist
        let config = Configuration::load_from_path("/nonexistent/ssamd.toml");
        assert!(config.is_err());
    }

    #[test]
    fn test_valid_config_file() {
        let mut temp_file = NamedTempFile::new().unwrap();
        let config_content = r#"
            [common]
            bundled_packages_dir = "/custom/base"
            downloaded_packages_dir = "/custom/packages"
            packages_data_root = "/custom/data"
            packages_overlayfs_root = "/custom/overlay"
            packages_mnt_root = "/custom/mnt"
            public_key_file_path = "/custom/key.pub"
            packages_cgroup = "custom.slice"
            packages_ext = "cpkg"
        "#;
        temp_file.write_all(config_content.as_bytes()).unwrap();
        temp_file.flush().unwrap();

        let config = Configuration::load_from_path(temp_file.path());
        assert!(config.is_ok());
        let config = config.unwrap();
        assert_eq!(config.bundled_packages_dir(), "/custom/base");
        assert_eq!(config.downloaded_packages_dir(), "/custom/packages");
    }

    #[test]
    fn test_invalid_toml() {
        let mut temp_file = NamedTempFile::new().unwrap();
        let invalid_content = r"
            [common]
            downloaded_packages_dir = [this is not valid toml
        ";
        temp_file.write_all(invalid_content.as_bytes()).unwrap();
        temp_file.flush().unwrap();

        let config = Configuration::load_from_path(temp_file.path());
        assert!(config.is_err());
    }

    #[test]
    fn test_partial_config() {
        let mut temp_file = NamedTempFile::new().unwrap();
        let config_content = r#"
            [common]
            downloaded_packages_dir = "/custom/packages"
            # Other fields are not specified - should fail due to missing required fields
        "#;
        temp_file.write_all(config_content.as_bytes()).unwrap();
        temp_file.flush().unwrap();

        let config = Configuration::load_from_path(temp_file.path());
        // Should fail because required fields are missing
        assert!(config.is_err());
        let err = config.unwrap_err();
        assert!(err.to_string().contains("Failed to parse config file"));
    }

    #[test]
    fn test_configuration_with_all_fields() {
        let mut temp_file = NamedTempFile::new().unwrap();
        let config_content = r#"
            [common]
            bundled_packages_dir = "/test/base"
            downloaded_packages_dir = "/test/packages"
            packages_data_root = "/test/data"
            packages_overlayfs_root = "/test/overlay"
            packages_mnt_root = "/test/mnt"
            public_key_file_path = "/test/key.pub"
            packages_cgroup = "test.slice"
            packages_ext = "tpkg"
        "#;
        temp_file.write_all(config_content.as_bytes()).unwrap();
        temp_file.flush().unwrap();

        let config = Configuration::load_from_path(temp_file.path()).unwrap();

        assert_eq!(config.bundled_packages_dir(), "/test/base");
        assert_eq!(config.downloaded_packages_dir(), "/test/packages");
        assert_eq!(config.packages_data_root(), "/test/data");
        assert_eq!(config.packages_overlayfs_root(), "/test/overlay");
        assert_eq!(config.packages_mnt_root(), "/test/mnt");
        assert_eq!(config.public_key_file_path(), "/test/key.pub");
        assert_eq!(config.packages_cgroup(), "test.slice");
        assert_eq!(config.packages_ext(), "tpkg");
    }

    #[test]
    fn test_configuration_getter_operations() {
        let temp_file = create_test_config_file();
        let config = Configuration::load_from_path(temp_file.path()).unwrap();

        let dir = config.downloaded_packages_dir();
        assert_eq!(dir.len(), "/test/packages".len());
        assert_eq!(dir.to_uppercase(), "/TEST/PACKAGES");
        assert!(std::path::Path::new(dir).is_absolute());
    }

    #[test]
    fn test_configuration_direct_access() {
        let temp_file = create_test_config_file();
        let config = Configuration::load_from_path(temp_file.path()).unwrap();

        let base_dir: &str = config.bundled_packages_dir();
        assert_eq!(base_dir, "/var/lib/ssamd/bundled");

        let dir: &str = config.downloaded_packages_dir();
        assert_eq!(dir, "/test/packages");

        let data_root: &str = config.packages_data_root();
        assert_eq!(data_root, "/test/data");
    }

    #[test]
    fn test_getter_no_allocation() {
        let temp_file = create_test_config_file();
        let config = Configuration::load_from_path(temp_file.path()).unwrap();

        let dir1 = config.downloaded_packages_dir();
        let dir2 = config.downloaded_packages_dir();

        assert_eq!(dir1, dir2);
        assert_eq!(dir1, "/test/packages");
    }

    #[test]
    fn test_configuration_clone() {
        let temp_file = create_test_config_file();
        let config1 = Configuration::load_from_path(temp_file.path()).unwrap();
        let config2 = config1.clone();

        assert_eq!(
            config1.downloaded_packages_dir(),
            config2.downloaded_packages_dir()
        );
        assert_eq!(
            config1.bundled_packages_dir(),
            config2.bundled_packages_dir()
        );
    }

    #[test]
    fn test_global_config_uses_testconfig() {
        ensure_test_init();

        let base_dir = bundled_packages_dir();
        let active_dir = downloaded_packages_dir();
        let data_root = packages_data_root();
        let overlayfs_root = packages_overlayfs_root();
        let mnt_root = packages_mnt_root();
        let key_path = public_key_file_path();
        let cgroup = packages_cgroup();
        let ext = packages_ext();

        assert_eq!(base_dir, "/var/lib/ssamd/bundled");
        assert_eq!(active_dir, "/var/lib/ssamd/downloaded");
        assert_eq!(data_root, "/var/lib/ssamd/data");
        assert_eq!(overlayfs_root, "");
        assert_eq!(mnt_root, "/var/lib/ssamd/mnt");
        assert_eq!(key_path, "/var/lib/ssamd/keys/test.pub.key");
        assert_eq!(cgroup, "");
        assert_eq!(ext, "ssam");
    }

    #[test]
    fn test_default_config_path_in_test_mode() {
        // Verify that default_config_path points to ssamd.toml in test environment
        let path = default_config_path();
        let expected = {
            let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
            manifest_dir.join("testdata/testconfig/ssamd.toml")
        };
        assert_eq!(path, expected);
        assert!(path.exists(), "Test config file should exist at {path:?}");
    }

    #[test]
    fn test_network_config_present() {
        let mut temp_file = NamedTempFile::new().unwrap();
        let config_content = r#"
            [common]
            bundled_packages_dir = "/var/lib/ssamd/bundled"
            downloaded_packages_dir = "/var/lib/ssamd/downloaded"
            packages_data_root = "/var/lib/ssamd/data"
            packages_overlayfs_root = ""
            packages_mnt_root = "/var/lib/ssamd/mnt"
            public_key_file_path = "/var/lib/ssamd/keys/test.pub.key"
            packages_cgroup = ""
            packages_ext = "ssam"

            [network.bridge]
            enabled = true

            [network.bridge.addr_pool]
            base = "172.20.0.0/16"
            size = 29
        "#;
        temp_file.write_all(config_content.as_bytes()).unwrap();
        temp_file.flush().unwrap();

        let config = Configuration::load_from_path(temp_file.path()).unwrap();
        let net = config.network_config();
        assert!(net.is_some());
        let bridge = net.unwrap().bridge.as_ref().unwrap();
        assert!(bridge.enabled);
        let pool = bridge.addr_pool.as_ref().unwrap();
        assert_eq!(pool.base.as_deref(), Some("172.20.0.0/16"));
        assert_eq!(pool.size, Some(29));
    }

    #[test]
    fn test_network_config_absent() {
        let mut temp_file = NamedTempFile::new().unwrap();
        let config_content = r#"
            [common]
            bundled_packages_dir = "/var/lib/ssamd/bundled"
            downloaded_packages_dir = "/var/lib/ssamd/downloaded"
            packages_data_root = "/var/lib/ssamd/data"
            packages_overlayfs_root = ""
            packages_mnt_root = "/var/lib/ssamd/mnt"
            public_key_file_path = "/var/lib/ssamd/keys/test.pub.key"
            packages_cgroup = ""
            packages_ext = "ssam"
        "#;
        temp_file.write_all(config_content.as_bytes()).unwrap();
        temp_file.flush().unwrap();

        let config = Configuration::load_from_path(temp_file.path()).unwrap();
        assert!(config.network_config().is_none());
    }

    #[test]
    fn test_network_config_optional_fields_none_when_omitted() {
        let mut temp_file = NamedTempFile::new().unwrap();
        let config_content = r#"
            [common]
            bundled_packages_dir = "/var/lib/ssamd/bundled"
            downloaded_packages_dir = "/var/lib/ssamd/downloaded"
            packages_data_root = "/var/lib/ssamd/data"
            packages_overlayfs_root = ""
            packages_mnt_root = "/var/lib/ssamd/mnt"
            public_key_file_path = "/var/lib/ssamd/keys/test.pub.key"
            packages_cgroup = ""
            packages_ext = "ssam"

            [network.bridge]
            enabled = true

            [network.bridge.addr_pool]
            base = "172.20.0.0/16"
        "#;
        temp_file.write_all(config_content.as_bytes()).unwrap();
        temp_file.flush().unwrap();

        let config = Configuration::load_from_path(temp_file.path()).unwrap();
        let bridge = config.bridge_config().unwrap();
        assert_eq!(bridge.addr_pool.as_ref().unwrap().size, None);
    }

    fn net_cfg(base: &str, size: Option<u8>) -> BridgeConfig {
        BridgeConfig {
            enabled: true,
            addr_pool: Some(PoolConfig {
                base: Some(base.to_owned()),
                size,
            }),
        }
    }

    #[test]
    fn network_validate_accepts_valid() {
        assert!(net_cfg("172.20.0.0/16", Some(29)).validate().is_ok());
        // An omitted size defaults to /29, still valid inside a /16.
        assert!(net_cfg("172.20.0.0/16", None).validate().is_ok());
    }

    #[test]
    fn base_defaults_when_omitted() {
        // No pool at all: base()/size() fall back to the built-in defaults.
        let bridge = BridgeConfig {
            enabled: true,
            addr_pool: None,
        };
        assert_eq!(bridge.base(), DEFAULT_POOL_BASE);
        assert!(bridge.validate().is_ok());
    }

    #[test]
    fn network_validate_rejects_bad_base() {
        let err = net_cfg("not-a-pool", Some(29)).validate().unwrap_err();
        assert!(err.to_string().contains("base"));
    }

    #[test]
    fn network_validate_rejects_pool_too_small_for_subnet() {
        // A /30 pool cannot hold a /29 subnet.
        let err = net_cfg("10.0.0.0/30", Some(29)).validate().unwrap_err();
        assert!(err.to_string().contains("too small"));
    }

    #[test]
    fn network_validate_rejects_prefix_with_no_host() {
        // /31 leaves no usable host address.
        let err = net_cfg("10.0.0.0/16", Some(31)).validate().unwrap_err();
        assert!(err.to_string().contains("usable host"));
    }

    #[test]
    fn load_rejects_invalid_network_section() {
        let mut temp_file = NamedTempFile::new().unwrap();
        let config_content = r#"
            [common]
            bundled_packages_dir = "/var/lib/ssamd/bundled"
            downloaded_packages_dir = "/var/lib/ssamd/downloaded"
            packages_data_root = "/var/lib/ssamd/data"
            packages_overlayfs_root = ""
            packages_mnt_root = "/var/lib/ssamd/mnt"
            public_key_file_path = "/var/lib/ssamd/keys/test.pub.key"
            packages_cgroup = ""
            packages_ext = "ssam"

            [network.bridge]
            enabled = true

            [network.bridge.addr_pool]
            base = "10.0.0.0/30"
            size = 29
        "#;
        temp_file.write_all(config_content.as_bytes()).unwrap();
        temp_file.flush().unwrap();

        // A pool too small for one subnet must fail at load, not at first attach.
        assert!(Configuration::load_from_path(temp_file.path()).is_err());
    }
}
