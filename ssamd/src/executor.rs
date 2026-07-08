// Copyright 2025-2026 Hyundai Mobis Co., Ltd.
// SPDX-License-Identifier: Apache-2.0

use std::sync::Arc;

#[non_exhaustive]
#[derive(strum_macros::Display, Debug)]
pub(crate) enum ContainerRuntime {
    #[strum(serialize = "/usr/bin/crun")]
    CRun,
}

#[derive(Debug)] // TODO:: Get default executor from config
pub(crate) enum ExecutorType {
    Systemd(systemd::ServiceInfo),
}

#[derive(Debug, Clone, derive_more::Deref)]
pub(crate) struct PackageExecutor(Arc<dyn CommandExecutorBackend>);

impl PackageExecutor {
    pub(crate) async fn new(
        package_name: String,
        command: Arc<dyn ExecuteCommand>,
        execution_type: ExecutorType,
        state_sender: tokio::sync::mpsc::Sender<ExecutionStatus>,
    ) -> anyhow::Result<Self> {
        let mgr = match execution_type {
            ExecutorType::Systemd(service_info) => systemd::TransientUnitExecutor::new(
                package_name,
                command,
                service_info,
                state_sender,
            )
            .await
            .map(|executor| Arc::new(executor) as Arc<dyn CommandExecutorBackend>),
        }?;
        Ok(Self(mgr))
    }
}

#[derive(Debug)]
pub(crate) enum ExecutionResult {
    Success(ExecutionStatus),
    Canceled(ExecutionStatus),
    Failure(String),
    Timeout,
}

pub(crate) trait ExecutionState: Send + Sync + std::fmt::Display + std::fmt::Debug {}

#[derive(Debug)]
pub(crate) enum ExecutionStatus {
    Active(Arc<dyn ExecutionState>),
    Inactive(Arc<dyn ExecutionState>),
}

impl std::fmt::Display for ExecutionStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = match self {
            ExecutionStatus::Active(state) | ExecutionStatus::Inactive(state) => state,
        };
        write!(f, "{state}")
    }
}

pub(crate) mod oci {
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};

    use anyhow::Context;
    use libssam::container::NetworkMode;
    use libssam::ssam_package::PackageSeccompPolicy;
    use libssam::utils::PrettyJsonWriter;
    use oci_spec::runtime::{
        Arch, Linux, LinuxNamespace, LinuxNamespaceBuilder, LinuxNamespaceType, LinuxSeccomp,
        Mount, MountBuilder, RootBuilder,
    };
    use tempfile::TempDir;

    /// Represents the host architecture mapped to seccomp architecture constants.
    /// Maps `std::env::consts::ARCH` values to corresponding `SCMP_ARCH_*` values.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) enum HostArch {
        /// x86 (32-bit) - maps to `SCMP_ARCH_X86`
        X86,
        /// `x86_64` (64-bit) - maps to `SCMP_ARCH_X86_64`
        X86_64,
        /// ARM (32-bit) - maps to `SCMP_ARCH_ARM`
        Arm,
        /// `AArch64` (64-bit) - maps to `SCMP_ARCH_AARCH64`
        Aarch64,
        /// Unsupported architecture
        Unsupported,
    }

    impl HostArch {
        /// Creates a `HostArch` from `std::env::consts::ARCH`.
        pub(crate) fn from_env() -> Self {
            match std::env::consts::ARCH {
                "x86" => Self::X86,
                "x86_64" => Self::X86_64,
                "arm" => Self::Arm,
                "aarch64" => Self::Aarch64,
                _ => Self::Unsupported,
            }
        }

        /// Returns the corresponding `oci_spec::runtime::Arch` value.
        pub(crate) fn to_oci_arch(self) -> Option<Arch> {
            match self {
                Self::X86 => Some(Arch::ScmpArchX86),
                Self::X86_64 => Some(Arch::ScmpArchX86_64),
                Self::Arm => Some(Arch::ScmpArchArm),
                Self::Aarch64 => Some(Arch::ScmpArchAarch64),
                Self::Unsupported => None,
            }
        }
    }

    #[derive(Debug)]
    pub(crate) struct TransientRuntimeConfig {
        path: TempDir,
    }

    impl TransientRuntimeConfig {
        const MOUNT_OPTION: [&'static str; 2] = ["rbind", "rw"];
        const CONTAINER_DEFAULT_APPARMOR_PROFILE: &str = "container-default";

        /// Creates a new transient runtime configuration for a container.
        ///
        /// Prepares a temporary directory with a `config.json` OCI runtime specification,
        /// configuring rootfs, seccomp, cgroups, and bind mounts for the package.
        ///
        /// # Arguments
        ///
        /// * `spec` - Owned snapshot of all inputs needed to build the bundle.
        pub(crate) fn new(spec: ContainerBundleSpec) -> anyhow::Result<Self> {
            let ContainerBundleSpec {
                oci_template,
                seccomp_policy,
                mac_enabled,
                network_mode,
                cgroups_path,
                package_name,
                mount_point,
                data_mounts,
                bridge_netns_path,
            } = spec;

            // Create bundle dir as 0700 atomically. /tmp is world-writable and the
            // default umask would leave it 0755, exposing config.json (mount paths,
            // env, args) and rootfs references to other local users.
            let path = tempfile::Builder::new()
                .permissions(std::fs::Permissions::from_mode(0o700))
                .tempdir()
                .context("Failed to create temporary directory")?;

            // just absolutize the rootfs path. the rootfs might not be mounted yet.
            let rootfs_path = std::path::absolute(&mount_point).with_context(|| {
                format!("Unable to absoluteize rootfs directory of {package_name}")
            })?;
            let mut oci_runtime_conf = oci_template;

            let linux = oci_runtime_conf.linux_mut().get_or_insert(Linux::default());

            let seccomp = Self::build_seccomp_config(seccomp_policy.as_ref());
            if seccomp.is_none() {
                log::info!("{package_name}: seccomp is disabled by configuration");
            }
            linux.set_seccomp(seccomp);

            let namespaces = linux.namespaces_mut().get_or_insert_with(Vec::new);
            Self::configure_network_namespace(
                namespaces,
                network_mode,
                &package_name,
                bridge_netns_path.as_deref(),
            )?;

            let process = oci_runtime_conf
                .process_mut()
                .get_or_insert(oci_spec::runtime::Process::default());

            let apparmor_profile = mac_enabled
                .then(|| Self::build_apparmor_profile(&package_name))
                .flatten();
            if apparmor_profile.is_none() {
                log::info!("{package_name}: AppArmor is disabled by configuration");
            }
            let _ = process.set_apparmor_profile(apparmor_profile);

            let cgroups_path =
                PathBuf::from(cgroups_path).join(format!("{package_name}.service/container"));
            Self::set_cgroups_path(&mut oci_runtime_conf, cgroups_path);

            let r = RootBuilder::default()
                .path(rootfs_path.clone())
                .readonly(true)
                .build()
                .context("Failed to build OCI root configuration")?;
            oci_runtime_conf.set_root(Some(r));

            if let Some(data_mounts) = data_mounts {
                let dirs: Vec<&Path> = data_mounts.dirs.iter().map(PathBuf::as_path).collect();
                match Self::make_bind_mounts(&data_mounts.root, dirs) {
                    Ok(bind_mounts) => {
                        for mnt in bind_mounts {
                            Self::append_mount(&mut oci_runtime_conf, mnt);
                        }
                    }
                    Err(e) => {
                        // Log the error but don't fail the entire operation
                        log::warn!("Warning: Failed to create bind mounts: {e}");
                    }
                }
            }

            let new_conf_path = path.path().join("config.json");

            oci_runtime_conf
                .save_pretty(new_conf_path.as_path())
                .context(format!(
                    "Failed to save container runtime spec to {}",
                    new_conf_path.display()
                ))?;

            Ok(Self { path })
        }

        // TODO: ssam-wrap will validate that runtime.json does not contain a
        // pre-configured network namespace when package config specifies network
        // mode. If both are present, ssam-wrap should reject the package at build
        // time rather than silently overriding here.
        fn configure_network_namespace(
            namespaces: &mut Vec<LinuxNamespace>,
            network_mode: NetworkMode,
            package_name: &str,
            bridge_netns_path: Option<&std::path::Path>,
        ) -> anyhow::Result<()> {
            namespaces.retain(|ns| ns.typ() != LinuxNamespaceType::Network);

            match network_mode {
                NetworkMode::Host => {
                    log::warn!(
                        "{package_name}: host network mode enabled, \
                         container network isolation disabled"
                    );
                }
                NetworkMode::None => {
                    let ns = LinuxNamespaceBuilder::default()
                        .typ(LinuxNamespaceType::Network)
                        .build()
                        .context("Failed to build network namespace entry")?;
                    namespaces.push(ns);
                }
                NetworkMode::Bridge => {
                    let path =
                        bridge_netns_path.context("bridge network mode requires a netns path")?;
                    let ns = LinuxNamespaceBuilder::default()
                        .typ(LinuxNamespaceType::Network)
                        .path(path.to_path_buf())
                        .build()
                        .context("Failed to build bridge network namespace entry")?;
                    namespaces.push(ns);
                }
            }
            Ok(())
        }

        pub(crate) fn set_cgroups_path(spec: &mut oci_spec::runtime::Spec, cgroups_path: PathBuf) {
            if let Some(linux) = spec.linux_mut() {
                linux.set_cgroups_path(Some(cgroups_path));
            }
        }

        pub(crate) fn append_mount(spec: &mut oci_spec::runtime::Spec, mount: Mount) {
            if let Some(mounts) = spec.mounts_mut() {
                mounts.push(mount);
            }
        }

        /// Builds the `AppArmor` profile name for the container.
        ///
        /// If `AppArmor` is not available on the host, returns `None`.
        /// Otherwise, returns the package-specific profile or falls back to the default.
        fn build_apparmor_profile(package_name: &str) -> Option<String> {
            let enabled = crate::apparmor::is_enabled()
                .inspect_err(|e| log::warn!("Failed to check AppArmor status: {e}"))
                .ok()?; // Err -> None, then early return

            if !enabled {
                log::info!("AppArmor is disabled on the host. Ignore to apply default profile.");
                return None;
            }

            let profile_name = format!("ssam-prof-{package_name}");
            let profile = crate::apparmor::get_profile(&profile_name)
                .inspect_err(|e| {
                    log::warn!(
                        "AppArmor may not be properly configured. \
                         Failed to get profile '{profile_name}': {e}"
                    );
                })
                .ok()?; // Err -> None, then early return

            profile.map(|_| profile_name).or_else(|| {
                log::warn!(
                    "AppArmor profile 'ssam-prof-{package_name}' not found. Falling back to default."
                );
                Some(Self::CONTAINER_DEFAULT_APPARMOR_PROFILE.to_string())
            })
        }

        /// Builds a `LinuxSeccomp` configuration from the given seccomp policy.
        ///
        /// Parses the JSON policy, extracts architecture mappings, and constructs
        /// the final `LinuxSeccomp` with appropriate architectures set.
        fn build_seccomp_config(
            seccomp_policy: Option<&PackageSeccompPolicy>,
        ) -> Option<LinuxSeccomp> {
            // Parse seccomp policy JSON
            let seccomp_json = seccomp_policy.and_then(|policy| {
                serde_json::from_slice::<serde_json::Value>(policy.as_bytes())
                    .inspect_err(|e| log::debug!("Failed to parse seccomp profile JSON: {e}"))
                    .ok()
            })?;

            // Extract architectures from archMap
            let architectures = seccomp_json
                .get("archMap")
                .and_then(|v| {
                    v.as_array().or_else(|| {
                        log::warn!("archMap is not an array in seccomp profile");
                        None
                    })
                })
                .and_then(|arch_map| {
                    Self::convert_arch_map_to_architectures(arch_map, HostArch::from_env())
                });

            // Build LinuxSeccomp and set architectures
            serde_json::from_value::<LinuxSeccomp>(seccomp_json)
                .inspect_err(|e| log::debug!("Failed to parse seccomp configuration: {e}"))
                .ok()
                .map(|mut seccomp| {
                    seccomp.set_architectures(architectures);
                    seccomp
                })
        }

        fn make_bind_mounts(
            package_data_root: &Path,
            data_dirs: Vec<&Path>,
        ) -> anyhow::Result<Vec<Mount>> {
            let dest_prefix = Path::new("/");
            data_dirs
                .into_iter()
                .map(|dest_path| {
                    let src_path =
                        package_data_root.join(dest_path.strip_prefix("/").unwrap_or(dest_path));
                    let dest = dest_prefix.join(dest_path);
                    let options = Self::MOUNT_OPTION
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>();
                    MountBuilder::default()
                        .destination(&dest)
                        .source(src_path)
                        .typ("none")
                        .options(options)
                        .build()
                        .map_err(Into::into)
                })
                .collect()
        }

        /// Converts archMap to a list of architectures based on the given host architecture.
        ///
        /// Returns `Some(archs)` if a matching entry is found,
        /// `None` if no match is found or if the host architecture is unsupported.
        pub(crate) fn convert_arch_map_to_architectures(
            arch_map: &[serde_json::Value],
            host_arch: HostArch,
        ) -> Option<Vec<Arch>> {
            let oci_arch = host_arch.to_oci_arch()?;
            let host_arch_str = oci_arch.to_string();

            // Find the entry matching the host architecture and parse architectures
            let architectures = arch_map.iter().find_map(|entry| {
                let arch_str = entry.get("architecture")?.as_str()?;
                if arch_str != host_arch_str {
                    return None;
                }

                let main_arch: Arch = arch_str.parse().ok()?;
                let sub_archs: Vec<Arch> = entry
                    .get("subArchitectures")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str().and_then(|s| s.parse().ok()))
                            .collect()
                    })
                    .unwrap_or_default();

                Some(std::iter::once(main_arch).chain(sub_archs).collect())
            });

            if architectures.is_none() {
                log::warn!("No matching architecture entry found in archMap for '{host_arch_str}'");
            }

            architectures
        }

        pub(crate) fn dir_path(&self) -> &Path {
            self.path.path()
        }
    }

    /// Owned snapshot of all inputs needed to create a container OCI bundle.
    /// No file I/O. Clone-able so it can be moved to a blocking thread.
    #[derive(Debug, Clone)]
    pub(crate) struct ContainerBundleSpec {
        pub(crate) oci_template: oci_spec::runtime::Spec,
        pub(crate) seccomp_policy: Option<PackageSeccompPolicy>,
        pub(crate) mac_enabled: bool,
        pub(crate) network_mode: NetworkMode,
        pub(crate) cgroups_path: String,
        pub(crate) package_name: String,
        pub(crate) mount_point: PathBuf,
        pub(crate) data_mounts: Option<DataMountPaths>,
        pub(crate) bridge_netns_path: Option<std::path::PathBuf>,
    }

    use crate::package_volume::{DataDirectory, QuotaEntryBackend};

    #[derive(Debug, Clone)]
    pub(crate) struct DataMountPaths {
        pub(crate) root: PathBuf,
        pub(crate) dirs: Vec<PathBuf>,
    }

    impl DataMountPaths {
        pub(crate) fn new<T: QuotaEntryBackend>(data_dir: &DataDirectory<T>) -> Self {
            Self {
                root: data_dir.path().to_path_buf(),
                dirs: data_dir
                    .data_dirs()
                    .unwrap_or_default()
                    .into_iter()
                    .map(Path::to_path_buf)
                    .collect(),
            }
        }
    }

    impl ContainerBundleSpec {
        /// Synchronous file I/O. Must be called inside `spawn_blocking`.
        pub(crate) fn into_runtime_config(self) -> anyhow::Result<TransientRuntimeConfig> {
            TransientRuntimeConfig::new(self)
        }
    }
}

#[async_trait::async_trait]
pub(crate) trait CommandExecutorBackend: Send + Sync + std::fmt::Debug {
    async fn start(&self) -> anyhow::Result<ExecutionResult>;
    async fn stop(&self) -> anyhow::Result<ExecutionResult>;
    async fn teardown(&self) -> anyhow::Result<()>;
}

#[async_trait::async_trait]
pub(crate) trait ExecuteCommand: Send + Sync + std::fmt::Debug {
    fn get_start_cmd(&self) -> anyhow::Result<Vec<String>>;
    fn get_stop_cmd(&self) -> anyhow::Result<Vec<String>>;
    async fn prepare(&self) -> anyhow::Result<()> {
        Ok(())
    }
}

pub(crate) mod container {
    use anyhow::Context as _;
    use libssam::container;
    use libssam::ssam_package::PackageFile;

    use crate::network::netns;
    use crate::package_volume::PackageVolume;

    use super::{ContainerRuntime, ExecuteCommand, oci};

    #[derive(Debug)]
    pub(crate) struct ContainerCommand {
        name: String,
        runtime: ContainerRuntime,
        spec: oci::ContainerBundleSpec,
        runtime_config: tokio::sync::OnceCell<oci::TransientRuntimeConfig>,
    }

    impl ContainerCommand {
        pub(crate) fn new(
            name: String,
            runtime: ContainerRuntime,
            spec: oci::ContainerBundleSpec,
        ) -> Self {
            Self {
                name,
                runtime,
                spec,
                runtime_config: tokio::sync::OnceCell::new(),
            }
        }

        /// Constructs a `ContainerCommand` from package metadata and volume.
        /// Extracts OCI config, security settings, network mode, and data paths.
        pub(crate) fn from_package(
            name: String,
            runtime: ContainerRuntime,
            package_file: &PackageFile,
            pkg_volume: &PackageVolume,
            network_mode: container::NetworkMode,
        ) -> Self {
            let metadata = package_file.metadata();

            let seccomp_policy = metadata
                .get_container_security_seccomp()
                .then(|| package_file.seccomp_policy().clone());

            let cgroups_path = super::systemd::cgroups_path().to_owned();

            let data_mounts = pkg_volume.data_directory().map(oci::DataMountPaths::new);

            let bridge_netns_path = if network_mode == container::NetworkMode::Bridge {
                Some(netns::netns_path(&netns::netns_name(&name)))
            } else {
                None
            };

            // Deref coercion: &PackageRuntimeConfig → &Box<Spec> → &Spec
            let oci_runtime_spec: &oci_spec::runtime::Spec = package_file.runtime_config();

            let spec = oci::ContainerBundleSpec {
                oci_template: oci_runtime_spec.clone(),
                seccomp_policy,
                mac_enabled: *metadata.get_container_security_mac(),
                network_mode,
                cgroups_path,
                package_name: name.clone(),
                mount_point: pkg_volume.get_mount_point().to_path_buf(),
                data_mounts,
                bridge_netns_path,
            };

            Self::new(name, runtime, spec)
        }

        #[cfg(test)]
        pub(crate) fn runtime_config(&self) -> &tokio::sync::OnceCell<oci::TransientRuntimeConfig> {
            &self.runtime_config
        }
    }

    #[async_trait::async_trait]
    impl ExecuteCommand for ContainerCommand {
        async fn prepare(&self) -> anyhow::Result<()> {
            self.runtime_config
                .get_or_try_init(|| async {
                    let spec = self.spec.clone();
                    tokio::task::spawn_blocking(move || spec.into_runtime_config())
                        .await
                        .context("join container bundle preparation task")?
                })
                .await?;
            Ok(())
        }

        fn get_start_cmd(&self) -> anyhow::Result<Vec<String>> {
            let path = self
                .runtime_config
                .get()
                .context("OCI runtime config not prepared")?
                .dir_path();
            if !path.exists() {
                anyhow::bail!("Bundle path does not exist: {}", path.display());
            }
            let bundle_path = path
                .to_str()
                .ok_or_else(|| anyhow::anyhow!("Invalid bundle path: {}", path.display()))?;
            Ok(vec![
                self.runtime.to_string(),
                "run".to_owned(),
                "--bundle".to_owned(),
                bundle_path.to_owned(),
                self.name.clone(),
            ])
        }

        fn get_stop_cmd(&self) -> anyhow::Result<Vec<String>> {
            Ok(vec![
                self.runtime.to_string(),
                "delete".to_owned(),
                "--force".to_owned(),
                self.name.clone(),
            ])
        }
    }
}

pub(crate) mod systemd;

#[cfg(test)]
mod tests {
    use super::oci::{ContainerBundleSpec, DataMountPaths, TransientRuntimeConfig};
    use crate::package_volume::{DataDirectory, PackageVolume};
    use libssam::ssam_package::PackageSeccompPolicy;
    use oci_spec::runtime::{LinuxBuilder, ProcessBuilder, RootBuilder, Spec};
    use std::fs;
    use std::path::{Path, PathBuf};
    use tempfile::TempDir;

    // Helper to create a test seccomp policy
    fn create_test_seccomp_policy() -> PackageSeccompPolicy {
        PackageSeccompPolicy::default_policy()
    }

    // Helper to create a real PackageVolume for testing using the package_volume test infrastructure
    fn create_test_package_volume_real(mount_point: &Path) -> anyhow::Result<PackageVolume> {
        use crate::package_volume::tests::mocks::{MockLoopDeviceControl, MockMountBackend};
        use crate::package_volume::tests::package_fs_metadata_test;
        use crate::package_volume::{PackageFileSystem, PackageFsMetadata};

        let package_name = "test-package".to_string();
        let test_pkg_file = package_fs_metadata_test::create_test_ssam_package_file();
        let pkgfs_meta = PackageFsMetadata::new(mount_point, &test_pkg_file)?;

        let pkgfs = PackageFileSystem {
            package_name: package_name.clone(),
            loop_controller: MockLoopDeviceControl,
            pkgfs_meta,
            mount_strategy: MockMountBackend,
        };

        Ok(PackageVolume::new(package_name, pkgfs, None))
    }

    // Helper to create a real PackageVolume with data directory for testing
    fn create_test_package_volume_real_with_data(
        mount_point: &Path,
        data_root: PathBuf,
        data_dirs_str: String,
    ) -> anyhow::Result<PackageVolume> {
        use crate::package_volume::tests::mocks::{MockLoopDeviceControl, MockMountBackend};
        use crate::package_volume::tests::package_fs_metadata_test;
        use crate::package_volume::{
            DefaultQuotaEntryBackend, PackageFileSystem, PackageFsMetadata,
        };

        let package_name = "test-package".to_string();
        let test_pkg_file = package_fs_metadata_test::create_test_ssam_package_file();
        let pkgfs_meta = PackageFsMetadata::new(mount_point, &test_pkg_file)?;

        let pkgfs = PackageFileSystem {
            package_name: package_name.clone(),
            loop_controller: MockLoopDeviceControl,
            pkgfs_meta,
            mount_strategy: MockMountBackend,
        };

        let data_directory =
            DataDirectory::<DefaultQuotaEntryBackend>::new(data_root, Some(data_dirs_str), None)?;

        Ok(PackageVolume::new(
            package_name,
            pkgfs,
            Some(data_directory),
        ))
    }

    #[test]
    fn test_transient_runtime_config_actual_new_function() {
        // Test the ACTUAL TransientRuntimeConfig::new function with real PackageVolume
        let oci_spec = create_test_oci_spec();
        let seccomp_policy = create_test_seccomp_policy();
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let mount_point = temp_dir.path().join("mount");
        fs::create_dir_all(&mount_point).expect("Failed to create mount point");

        // Create a real PackageVolume using the same infrastructure as package_volume tests
        let package_volume = create_test_package_volume_real(&mount_point)
            .expect("Failed to create test PackageVolume");

        // Call the ACTUAL TransientRuntimeConfig::new function
        let result = TransientRuntimeConfig::new(ContainerBundleSpec {
            oci_template: oci_spec,
            seccomp_policy: Some(seccomp_policy),
            mac_enabled: true,
            network_mode: libssam::container::NetworkMode::None,
            cgroups_path: "/sys/fs/cgroup/system.slice".to_owned(),
            package_name: "test-package".to_owned(),
            mount_point: package_volume.get_mount_point().to_path_buf(),
            data_mounts: None,
            bridge_netns_path: None,
        });

        assert!(
            result.is_ok(),
            "Actual TransientRuntimeConfig::new should succeed: {:?}",
            result.err()
        );

        let config = result.unwrap();
        let config_path = config.dir_path();
        assert!(config_path.exists(), "Config directory should exist");

        // Verify the config.json was created
        let config_file = config_path.join("config.json");
        assert!(config_file.exists(), "config.json should be created");

        // Verify the content is valid JSON
        let content = fs::read_to_string(&config_file).expect("Should read config file");

        // Simple JSON validation - check for basic structure
        assert!(
            content.contains('{') && content.contains('}'),
            "Should be valid JSON structure"
        );

        // Verify it contains expected elements
        assert!(
            content.contains("test-package"),
            "Should contain package name as hostname"
        );
        assert!(
            content.contains("/sys/fs/cgroup/system.slice/test-package.service/container"),
            "Should contain correct cgroups path"
        );
    }

    #[test]
    fn test_transient_runtime_config_bundle_dir_is_0700() {
        use std::os::unix::fs::PermissionsExt;

        let oci_spec = create_test_oci_spec();
        let seccomp_policy = create_test_seccomp_policy();
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let mount_point = temp_dir.path().join("mount");
        fs::create_dir_all(&mount_point).expect("Failed to create mount point");

        let package_volume = create_test_package_volume_real(&mount_point)
            .expect("Failed to create test PackageVolume");

        let config = TransientRuntimeConfig::new(ContainerBundleSpec {
            oci_template: oci_spec,
            seccomp_policy: Some(seccomp_policy),
            mac_enabled: true,
            network_mode: libssam::container::NetworkMode::None,
            cgroups_path: "/sys/fs/cgroup/system.slice".to_owned(),
            package_name: "test-package".to_owned(),
            mount_point: package_volume.get_mount_point().to_path_buf(),
            data_mounts: None,
            bridge_netns_path: None,
        })
        .expect("TransientRuntimeConfig::new should succeed");

        let meta = fs::symlink_metadata(config.dir_path()).expect("Failed to stat bundle dir");
        assert_eq!(
            meta.permissions().mode() & 0o777,
            0o700,
            "Bundle directory must be restricted to 0700"
        );
    }

    #[test]
    fn test_transient_runtime_config_actual_new_with_data_directory() {
        // Test the ACTUAL function with data directories
        let oci_spec = create_test_oci_spec();
        let seccomp_policy = create_test_seccomp_policy();
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let mount_point = temp_dir.path().join("mount");
        let data_root = temp_dir.path().join("data");
        fs::create_dir_all(&mount_point).expect("Failed to create mount point");
        fs::create_dir_all(&data_root).expect("Failed to create data root");

        // Create a real PackageVolume with data directory
        let package_volume = create_test_package_volume_real_with_data(
            &mount_point,
            data_root,
            "/app/data:/var/log".to_string(),
        )
        .expect("Failed to create test PackageVolume with data");

        let data_mounts = package_volume.data_directory().map(DataMountPaths::new);

        // Call the ACTUAL TransientRuntimeConfig::new function
        let result = TransientRuntimeConfig::new(ContainerBundleSpec {
            oci_template: oci_spec,
            seccomp_policy: Some(seccomp_policy),
            mac_enabled: true,
            network_mode: libssam::container::NetworkMode::None,
            cgroups_path: "/sys/fs/cgroup/system.slice".to_owned(),
            package_name: "test-package".to_owned(),
            mount_point: package_volume.get_mount_point().to_path_buf(),
            data_mounts,
            bridge_netns_path: None,
        });

        assert!(
            result.is_ok(),
            "Actual TransientRuntimeConfig::new with data dirs should succeed: {:?}",
            result.err()
        );

        let config = result.unwrap();
        let config_file = config.dir_path().join("config.json");
        let content = fs::read_to_string(&config_file).expect("Should read config file");

        // Verify bind mounts are created for data directories
        // Note: Linux bind mounts typically use type "none" with "rbind" option
        assert!(
            content.contains("\"type\": \"none\""),
            "Should contain type: none (with space after colon)"
        );
        assert!(content.contains("rbind"), "Should contain rbind option");
        assert!(
            content.contains("app/data") && content.contains("var/log"),
            "Should contain both data directory paths"
        );
    }

    #[test]
    fn test_transient_runtime_config_seccomp_disabled() {
        // Test that seccomp is not applied when seccomp policy is None
        let oci_spec = create_test_oci_spec();
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let mount_point = temp_dir.path().join("mount");
        fs::create_dir_all(&mount_point).expect("Failed to create mount point");

        let package_volume = create_test_package_volume_real(&mount_point)
            .expect("Failed to create test PackageVolume");

        // Call TransientRuntimeConfig::new with seccomp_policy = None
        let result = TransientRuntimeConfig::new(ContainerBundleSpec {
            oci_template: oci_spec,
            seccomp_policy: None,
            mac_enabled: true,
            network_mode: libssam::container::NetworkMode::None,
            cgroups_path: "/sys/fs/cgroup/system.slice".to_owned(),
            package_name: "test-package-no-seccomp".to_owned(),
            mount_point: package_volume.get_mount_point().to_path_buf(),
            data_mounts: None,
            bridge_netns_path: None,
        });

        assert!(
            result.is_ok(),
            "TransientRuntimeConfig::new with seccomp disabled should succeed: {:?}",
            result.err()
        );

        let config = result.unwrap();
        let config_file = config.dir_path().join("config.json");
        let content = fs::read_to_string(&config_file).expect("Should read config file");

        // Verify seccomp is not present in the config
        let config_json: serde_json::Value =
            serde_json::from_str(&content).expect("Should parse config.json");
        let linux = config_json.get("linux").expect("Should have linux section");
        let seccomp = linux.get("seccomp");

        assert!(
            seccomp.is_none() || seccomp == Some(&serde_json::Value::Null),
            "seccomp should be null or absent when disabled, but got: {seccomp:?}"
        );
    }

    #[test]
    fn test_transient_runtime_config_seccomp() {
        // Test that seccomp is applied when seccomp policy is provided
        let oci_spec = create_test_oci_spec();
        let seccomp_policy = create_test_seccomp_policy();
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let mount_point = temp_dir.path().join("mount");
        fs::create_dir_all(&mount_point).expect("Failed to create mount point");

        let package_volume = create_test_package_volume_real(&mount_point)
            .expect("Failed to create test PackageVolume");

        // Call TransientRuntimeConfig::new with seccomp_policy = Some
        let result = TransientRuntimeConfig::new(ContainerBundleSpec {
            oci_template: oci_spec,
            seccomp_policy: Some(seccomp_policy),
            mac_enabled: true,
            network_mode: libssam::container::NetworkMode::None,
            cgroups_path: "/sys/fs/cgroup/system.slice".to_owned(),
            package_name: "test-package-with-seccomp".to_owned(),
            mount_point: package_volume.get_mount_point().to_path_buf(),
            data_mounts: None,
            bridge_netns_path: None,
        });

        assert!(
            result.is_ok(),
            "TransientRuntimeConfig::new with seccomp enabled should succeed: {:?}",
            result.err()
        );

        let config = result.unwrap();
        let config_file = config.dir_path().join("config.json");
        let content = fs::read_to_string(&config_file).expect("Should read config file");

        // Verify seccomp is present in the config
        let config_json: serde_json::Value =
            serde_json::from_str(&content).expect("Should parse config.json");
        let linux = config_json.get("linux").expect("Should have linux section");
        let seccomp = linux.get("seccomp");

        assert!(
            seccomp.is_some() && seccomp != Some(&serde_json::Value::Null),
            "seccomp should be present when enabled"
        );

        // Verify seccomp has expected structure
        let seccomp = seccomp.unwrap();
        assert!(
            seccomp.get("defaultAction").is_some(),
            "seccomp should have defaultAction"
        );
    }

    fn create_test_oci_spec() -> Spec {
        let process = ProcessBuilder::default()
            .args(vec!["sh".to_string()])
            .build()
            .expect("Failed to build process");

        let root = RootBuilder::default()
            .path("/")
            .build()
            .expect("Failed to build root");

        let linux = LinuxBuilder::default()
            .build()
            .expect("Failed to build linux config");

        oci_spec::runtime::SpecBuilder::default()
            .version("1.0.2")
            .process(process)
            .root(root)
            .linux(linux)
            .build()
            .expect("Failed to build OCI spec")
    }

    // Tests for ContainerExecutor
    mod container_command_tests {
        use super::*;
        use crate::executor::container::ContainerCommand;
        use crate::executor::{ContainerRuntime, ExecuteCommand};

        fn create_test_container_command() -> ContainerCommand {
            use crate::executor::oci::ContainerBundleSpec;
            let oci_spec = create_test_oci_spec();
            let seccomp_policy = create_test_seccomp_policy();
            let temp_dir = TempDir::new().expect("Failed to create temp dir");
            let mount_point = temp_dir.path().join("mount");
            fs::create_dir_all(&mount_point).expect("Failed to create mount point");

            let spec = ContainerBundleSpec {
                oci_template: oci_spec,
                seccomp_policy: Some(seccomp_policy),
                mac_enabled: true,
                network_mode: libssam::container::NetworkMode::None,
                cgroups_path: "/sys/fs/cgroup/system.slice".to_owned(),
                package_name: "test-container".to_owned(),
                mount_point,
                data_mounts: None,
                bridge_netns_path: None,
            };
            // temp_dir drops here but mount_point PathBuf is copied into spec.
            // std::path::absolute() does not require path to exist on disk.
            ContainerCommand::new("test-container".to_string(), ContainerRuntime::CRun, spec)
        }

        #[test]
        fn test_container_command_new() {
            let executor = create_test_container_command();

            // Test that the ContainerExecutor was created successfully
            assert!(format!("{executor:?}").contains("test-container"));
            assert!(format!("{executor:?}").contains("CRun"));
        }

        #[tokio::test]
        async fn test_container_command_get_start_args() {
            let executor = create_test_container_command();

            executor
                .prepare()
                .await
                .expect("command prepare should succeed");

            let start_args = executor.get_start_cmd().expect("Should get start command");

            assert_eq!(start_args.len(), 5);
            assert_eq!(start_args[0], "/usr/bin/crun");
            assert_eq!(start_args[1], "run");
            assert_eq!(start_args[2], "--bundle");
            assert_eq!(start_args[4], "test-container");

            let bundle_path = Path::new(&start_args[3]);
            assert!(
                bundle_path.exists(),
                "Bundle path should exist: {bundle_path:?}"
            );
        }

        #[test]
        fn test_container_command_get_stop_args() {
            let executor = create_test_container_command();

            let stop_args = executor.get_stop_cmd().expect("Should get stop command");

            // Verify the stop command structure
            assert_eq!(stop_args.len(), 4);
            assert_eq!(stop_args[0], "/usr/bin/crun");
            assert_eq!(stop_args[1], "delete");
            assert_eq!(stop_args[2], "--force");
            assert_eq!(stop_args[3], "test-container");
        }

        #[tokio::test]
        async fn test_container_command_get_start_args_bundle_not_exists() {
            let executor = create_test_container_command();

            executor
                .prepare()
                .await
                .expect("command prepare should succeed");

            let bundle_path = executor
                .runtime_config()
                .get()
                .expect("bundle should be prepared")
                .dir_path()
                .to_path_buf();
            if bundle_path.exists() {
                fs::remove_dir_all(&bundle_path).expect("Failed to remove bundle dir");
            }

            let result = executor.get_start_cmd();
            assert!(
                result.is_err(),
                "Should fail when bundle path doesn't exist"
            );
            let error_msg = result.unwrap_err().to_string();
            assert!(
                error_msg.contains("Bundle path does not exist"),
                "Error should mention bundle path: {error_msg}"
            );
        }

        #[test]
        fn test_container_command_unprepared_get_start_args_error() {
            let executor = create_test_container_command();

            let result = executor.get_start_cmd();

            assert!(result.is_err(), "Should fail before prepare");
            let error_msg = result.unwrap_err().to_string();
            assert!(
                error_msg.contains("not prepared"),
                "Error should mention preparation state: {error_msg}"
            );
        }

        #[tokio::test]
        async fn test_container_command_prepare_idempotent() {
            let executor = create_test_container_command();

            executor
                .prepare()
                .await
                .expect("first prepare should succeed");
            let first_path = executor
                .runtime_config()
                .get()
                .expect("bundle should be prepared")
                .dir_path()
                .to_path_buf();

            executor
                .prepare()
                .await
                .expect("second prepare should succeed");
            let second_path = executor
                .runtime_config()
                .get()
                .expect("bundle should stay prepared")
                .dir_path()
                .to_path_buf();

            assert_eq!(first_path, second_path);
        }

        #[tokio::test]
        async fn test_container_command_prepared_get_start_args_valid() {
            let executor = create_test_container_command();

            executor
                .prepare()
                .await
                .expect("command prepare should succeed");
            let start_args = executor.get_start_cmd().expect("Should get start command");

            assert_eq!(start_args[0], "/usr/bin/crun");
            assert_eq!(start_args[1], "run");
            assert_eq!(start_args[2], "--bundle");
            assert!(Path::new(&start_args[3]).exists());
            assert_eq!(start_args[4], "test-container");
        }

        #[test]
        fn test_container_runtime_display() {
            let runtime = ContainerRuntime::CRun;
            assert_eq!(runtime.to_string(), "/usr/bin/crun");
        }

        #[test]
        fn test_container_command_debug_format() {
            let executor = create_test_container_command();
            let debug_str = format!("{executor:?}");

            // Verify debug format contains expected information
            assert!(debug_str.contains("ContainerCommand"));
            assert!(debug_str.contains("test-container"));
            assert!(debug_str.contains("CRun"));
            assert!(debug_str.contains("ContainerBundleSpec"));
        }
    }

    // Tests for ExecutorImpl
    mod executor_impl_tests {

        use crate::executor::ContainerRuntime;

        #[test]
        fn test_container_runtime_variants() {
            // Test ContainerRuntime enum
            let crun = ContainerRuntime::CRun;
            assert_eq!(crun.to_string(), "/usr/bin/crun");
            assert_eq!(format!("{crun:?}"), "CRun");
        }

        #[test]
        fn test_execution_result_variants() {
            // Test ExecutionResult enum variants
            use crate::executor::ExecutionResult;

            // Test variant creation - we can only test Success and Failure since they don't require real state
            let failure_result = ExecutionResult::Failure("Test failure".to_string());
            assert_eq!(format!("{failure_result:?}"), "Failure(\"Test failure\")");

            let timeout_result = ExecutionResult::Timeout;
            assert_eq!(format!("{timeout_result:?}"), "Timeout");
        }
    }

    mod host_arch_tests {
        use crate::executor::oci::HostArch;
        use oci_spec::runtime::Arch;

        #[test]
        fn test_host_arch_to_oci_arch() {
            assert_eq!(HostArch::X86.to_oci_arch(), Some(Arch::ScmpArchX86));
            assert_eq!(HostArch::X86_64.to_oci_arch(), Some(Arch::ScmpArchX86_64));
            assert_eq!(HostArch::Arm.to_oci_arch(), Some(Arch::ScmpArchArm));
            assert_eq!(HostArch::Aarch64.to_oci_arch(), Some(Arch::ScmpArchAarch64));
            assert_eq!(HostArch::Unsupported.to_oci_arch(), None);
        }

        #[test]
        fn test_oci_arch_to_string() {
            // Verify that oci_spec::runtime::Arch produces expected SCMP_ARCH_* strings
            assert_eq!(Arch::ScmpArchX86.to_string(), "SCMP_ARCH_X86");
            assert_eq!(Arch::ScmpArchX86_64.to_string(), "SCMP_ARCH_X86_64");
            assert_eq!(Arch::ScmpArchArm.to_string(), "SCMP_ARCH_ARM");
            assert_eq!(Arch::ScmpArchAarch64.to_string(), "SCMP_ARCH_AARCH64");
        }
    }

    mod convert_arch_map_tests {
        use crate::executor::oci::{HostArch, TransientRuntimeConfig};
        use oci_spec::runtime::Arch;
        use serde_json::json;

        fn create_test_arch_map() -> serde_json::Value {
            json!([
                {
                    "architecture": "SCMP_ARCH_X86_64",
                    "subArchitectures": [
                        "SCMP_ARCH_X86",
                        "SCMP_ARCH_X32"
                    ]
                },
                {
                    "architecture": "SCMP_ARCH_AARCH64",
                    "subArchitectures": [
                        "SCMP_ARCH_ARM"
                    ]
                },
                {
                    "architecture": "SCMP_ARCH_X86",
                    "subArchitectures": []
                },
                {
                    "architecture": "SCMP_ARCH_ARM",
                    "subArchitectures": []
                }
            ])
        }

        #[test]
        fn test_convert_arch_map_x86_64() {
            let arch_map = create_test_arch_map();
            let arch_map_arr = arch_map.as_array().unwrap();
            let result = TransientRuntimeConfig::convert_arch_map_to_architectures(
                arch_map_arr,
                HostArch::X86_64,
            );

            assert!(result.is_some());
            let archs = result.unwrap();
            assert_eq!(archs.len(), 3);
            assert_eq!(archs[0], Arch::ScmpArchX86_64);
            assert_eq!(archs[1], Arch::ScmpArchX86);
            assert_eq!(archs[2], Arch::ScmpArchX32);
        }

        #[test]
        fn test_convert_arch_map_aarch64() {
            let arch_map = create_test_arch_map();
            let arch_map_arr = arch_map.as_array().unwrap();
            let result = TransientRuntimeConfig::convert_arch_map_to_architectures(
                arch_map_arr,
                HostArch::Aarch64,
            );

            assert!(result.is_some());
            let archs = result.unwrap();
            assert_eq!(archs.len(), 2);
            assert_eq!(archs[0], Arch::ScmpArchAarch64);
            assert_eq!(archs[1], Arch::ScmpArchArm);
        }

        #[test]
        fn test_convert_arch_map_x86() {
            let arch_map = create_test_arch_map();
            let arch_map_arr = arch_map.as_array().unwrap();
            let result = TransientRuntimeConfig::convert_arch_map_to_architectures(
                arch_map_arr,
                HostArch::X86,
            );

            assert!(result.is_some());
            let archs = result.unwrap();
            assert_eq!(archs.len(), 1);
            assert_eq!(archs[0], Arch::ScmpArchX86);
        }

        #[test]
        fn test_convert_arch_map_arm() {
            let arch_map = create_test_arch_map();
            let arch_map_arr = arch_map.as_array().unwrap();
            let result = TransientRuntimeConfig::convert_arch_map_to_architectures(
                arch_map_arr,
                HostArch::Arm,
            );

            assert!(result.is_some());
            let archs = result.unwrap();
            assert_eq!(archs.len(), 1);
            assert_eq!(archs[0], Arch::ScmpArchArm);
        }

        #[test]
        fn test_convert_arch_map_unsupported() {
            let arch_map = create_test_arch_map();
            let arch_map_arr = arch_map.as_array().unwrap();
            let result = TransientRuntimeConfig::convert_arch_map_to_architectures(
                arch_map_arr,
                HostArch::Unsupported,
            );

            assert!(result.is_none());
        }

        #[test]
        fn test_convert_arch_map_no_matching_entry() {
            // Create archMap without matching architecture
            let arch_map = json!([
                {
                    "architecture": "SCMP_ARCH_MIPS",
                    "subArchitectures": []
                }
            ]);
            let arch_map_arr = arch_map.as_array().unwrap();

            let result = TransientRuntimeConfig::convert_arch_map_to_architectures(
                arch_map_arr,
                HostArch::X86_64,
            );
            assert!(result.is_none());
        }

        #[test]
        fn test_convert_arch_map_without_sub_architectures() {
            let arch_map = json!([
                {
                    "architecture": "SCMP_ARCH_X86_64"
                }
            ]);
            let arch_map_arr = arch_map.as_array().unwrap();

            let result = TransientRuntimeConfig::convert_arch_map_to_architectures(
                arch_map_arr,
                HostArch::X86_64,
            );
            assert!(result.is_some());
            let archs = result.unwrap();
            assert_eq!(archs.len(), 1);
            assert_eq!(archs[0], Arch::ScmpArchX86_64);
        }

        #[test]
        fn test_convert_arch_map_with_empty_sub_architectures() {
            let arch_map = json!([
                {
                    "architecture": "SCMP_ARCH_X86_64",
                    "subArchitectures": []
                }
            ]);
            let arch_map_arr = arch_map.as_array().unwrap();

            let result = TransientRuntimeConfig::convert_arch_map_to_architectures(
                arch_map_arr,
                HostArch::X86_64,
            );
            assert!(result.is_some());
            let archs = result.unwrap();
            assert_eq!(archs.len(), 1);
        }

        #[test]
        fn test_convert_arch_map_with_invalid_sub_architecture() {
            // Invalid sub-architecture should be filtered out
            let arch_map = json!([
                {
                    "architecture": "SCMP_ARCH_X86_64",
                    "subArchitectures": [
                        "SCMP_ARCH_X86",
                        "INVALID_ARCH",
                        "SCMP_ARCH_X32"
                    ]
                }
            ]);
            let arch_map_arr = arch_map.as_array().unwrap();

            let result = TransientRuntimeConfig::convert_arch_map_to_architectures(
                arch_map_arr,
                HostArch::X86_64,
            );
            assert!(result.is_some());
            let archs = result.unwrap();
            // INVALID_ARCH should be filtered out
            assert_eq!(archs.len(), 3);
            assert_eq!(archs[0], Arch::ScmpArchX86_64);
            assert_eq!(archs[1], Arch::ScmpArchX86);
            assert_eq!(archs[2], Arch::ScmpArchX32);
        }

        #[test]
        fn test_convert_arch_map_empty_array() {
            let arch_map: Vec<serde_json::Value> = vec![];

            let result = TransientRuntimeConfig::convert_arch_map_to_architectures(
                &arch_map,
                HostArch::X86_64,
            );
            assert!(result.is_none());
        }
    }

    mod network_mode_tests {
        use super::*;
        use libssam::container::NetworkMode;
        use oci_spec::runtime::{LinuxNamespaceBuilder, LinuxNamespaceType, SpecBuilder};

        fn create_test_oci_spec_with_network_ns() -> Spec {
            let process = ProcessBuilder::default()
                .args(vec!["sh".to_string()])
                .build()
                .expect("Failed to build process");

            let root = RootBuilder::default()
                .path("/")
                .build()
                .expect("Failed to build root");

            let namespaces = vec![
                LinuxNamespaceBuilder::default()
                    .typ(LinuxNamespaceType::Pid)
                    .build()
                    .expect("Failed to build pid namespace"),
                LinuxNamespaceBuilder::default()
                    .typ(LinuxNamespaceType::Network)
                    .build()
                    .expect("Failed to build network namespace"),
                LinuxNamespaceBuilder::default()
                    .typ(LinuxNamespaceType::Mount)
                    .build()
                    .expect("Failed to build mount namespace"),
            ];

            let linux = LinuxBuilder::default()
                .namespaces(namespaces)
                .build()
                .expect("Failed to build linux config");

            SpecBuilder::default()
                .version("1.0.2")
                .process(process)
                .root(root)
                .linux(linux)
                .build()
                .expect("Failed to build OCI spec")
        }

        #[test]
        fn test_host_network_mode_removes_network_namespace() {
            let oci_spec = create_test_oci_spec_with_network_ns();
            let temp_dir = TempDir::new().expect("Failed to create temp dir");
            let mount_point = temp_dir.path().join("mount");
            fs::create_dir_all(&mount_point).expect("Failed to create mount point");

            let package_volume = create_test_package_volume_real(&mount_point)
                .expect("Failed to create test PackageVolume");

            let result = TransientRuntimeConfig::new(ContainerBundleSpec {
                oci_template: oci_spec,
                seccomp_policy: None,
                mac_enabled: false,
                network_mode: NetworkMode::Host,
                cgroups_path: "/sys/fs/cgroup/system.slice".to_owned(),
                package_name: "test-host-net".to_owned(),
                mount_point: package_volume.get_mount_point().to_path_buf(),
                data_mounts: None,
                bridge_netns_path: None,
            });

            assert!(result.is_ok(), "Should succeed: {:?}", result.err());
            let config = result.unwrap();
            let config_file = config.dir_path().join("config.json");
            let content = fs::read_to_string(&config_file).expect("Should read config file");
            let config_json: serde_json::Value =
                serde_json::from_str(&content).expect("Should parse config.json");

            let linux = config_json.get("linux").expect("Should have linux section");
            let namespaces = linux
                .get("namespaces")
                .and_then(|v| v.as_array())
                .expect("Should have namespaces");

            let has_network_ns = namespaces.iter().any(|ns| {
                ns.get("type")
                    .and_then(|t| t.as_str())
                    .is_some_and(|t| t == "network")
            });
            assert!(
                !has_network_ns,
                "Host mode should remove network namespace, got: {namespaces:?}"
            );

            let has_pid_ns = namespaces.iter().any(|ns| {
                ns.get("type")
                    .and_then(|t| t.as_str())
                    .is_some_and(|t| t == "pid")
            });
            assert!(has_pid_ns, "Non-network namespaces should remain");
        }

        #[test]
        fn test_none_network_mode_ensures_network_namespace_present() {
            let oci_spec = create_test_oci_spec();
            let temp_dir = TempDir::new().expect("Failed to create temp dir");
            let mount_point = temp_dir.path().join("mount");
            fs::create_dir_all(&mount_point).expect("Failed to create mount point");

            let package_volume = create_test_package_volume_real(&mount_point)
                .expect("Failed to create test PackageVolume");

            let result = TransientRuntimeConfig::new(ContainerBundleSpec {
                oci_template: oci_spec,
                seccomp_policy: None,
                mac_enabled: false,
                network_mode: NetworkMode::None,
                cgroups_path: "/sys/fs/cgroup/system.slice".to_owned(),
                package_name: "test-none-net".to_owned(),
                mount_point: package_volume.get_mount_point().to_path_buf(),
                data_mounts: None,
                bridge_netns_path: None,
            });

            assert!(result.is_ok(), "Should succeed: {:?}", result.err());
            let config = result.unwrap();
            let config_file = config.dir_path().join("config.json");
            let content = fs::read_to_string(&config_file).expect("Should read config file");
            let config_json: serde_json::Value =
                serde_json::from_str(&content).expect("Should parse config.json");

            let linux = config_json.get("linux").expect("Should have linux section");
            let namespaces = linux
                .get("namespaces")
                .and_then(|v| v.as_array())
                .expect("None mode should create namespaces list");

            let has_network_ns = namespaces.iter().any(|ns| {
                ns.get("type")
                    .and_then(|t| t.as_str())
                    .is_some_and(|t| t == "network")
            });
            assert!(
                has_network_ns,
                "None mode must ensure network namespace exists for isolation, got: {namespaces:?}"
            );
        }

        #[test]
        fn test_none_network_mode_does_not_duplicate_existing_namespace() {
            let oci_spec = create_test_oci_spec_with_network_ns();
            let temp_dir = TempDir::new().expect("Failed to create temp dir");
            let mount_point = temp_dir.path().join("mount");
            fs::create_dir_all(&mount_point).expect("Failed to create mount point");

            let package_volume = create_test_package_volume_real(&mount_point)
                .expect("Failed to create test PackageVolume");

            let result = TransientRuntimeConfig::new(ContainerBundleSpec {
                oci_template: oci_spec,
                seccomp_policy: None,
                mac_enabled: false,
                network_mode: NetworkMode::None,
                cgroups_path: "/sys/fs/cgroup/system.slice".to_owned(),
                package_name: "test-none-dup".to_owned(),
                mount_point: package_volume.get_mount_point().to_path_buf(),
                data_mounts: None,
                bridge_netns_path: None,
            });

            assert!(result.is_ok(), "Should succeed: {:?}", result.err());
            let config = result.unwrap();
            let config_file = config.dir_path().join("config.json");
            let content = fs::read_to_string(&config_file).expect("Should read config file");
            let config_json: serde_json::Value =
                serde_json::from_str(&content).expect("Should parse config.json");

            let linux = config_json.get("linux").expect("Should have linux section");
            let namespaces = linux
                .get("namespaces")
                .and_then(|v| v.as_array())
                .expect("Should have namespaces");

            let network_ns_count = namespaces
                .iter()
                .filter(|ns| {
                    ns.get("type")
                        .and_then(|t| t.as_str())
                        .is_some_and(|t| t == "network")
                })
                .count();
            assert_eq!(
                network_ns_count, 1,
                "Should not duplicate existing network namespace"
            );
        }

        #[test]
        fn bridge_sets_path() {
            let oci_spec = create_test_oci_spec();
            let temp_dir = TempDir::new().expect("Failed to create temp dir");
            let mount_point = temp_dir.path().join("mount");
            fs::create_dir_all(&mount_point).expect("Failed to create mount point");

            let package_volume = create_test_package_volume_real(&mount_point)
                .expect("Failed to create test PackageVolume");

            let netns_path = std::path::Path::new("/run/netns/ssam-test");
            let result = TransientRuntimeConfig::new(ContainerBundleSpec {
                oci_template: oci_spec,
                seccomp_policy: None,
                mac_enabled: false,
                network_mode: NetworkMode::Bridge,
                cgroups_path: "/sys/fs/cgroup/system.slice".to_owned(),
                package_name: "test-bridge-path".to_owned(),
                mount_point: package_volume.get_mount_point().to_path_buf(),
                data_mounts: None,
                bridge_netns_path: Some(netns_path.to_path_buf()),
            });

            assert!(
                result.is_ok(),
                "Bridge with netns path should succeed: {:?}",
                result.err()
            );
            let config = result.unwrap();
            let config_file = config.dir_path().join("config.json");
            let content = fs::read_to_string(&config_file).expect("Should read config file");
            let config_json: serde_json::Value =
                serde_json::from_str(&content).expect("Should parse config.json");

            let linux = config_json.get("linux").expect("Should have linux section");
            let namespaces = linux
                .get("namespaces")
                .and_then(|v| v.as_array())
                .expect("Bridge mode should produce namespaces list");

            let net_ns = namespaces.iter().find(|ns| {
                ns.get("type")
                    .and_then(|t| t.as_str())
                    .is_some_and(|t| t == "network")
            });
            assert!(
                net_ns.is_some(),
                "Bridge mode must add a network namespace entry"
            );
            let ns_path = net_ns
                .unwrap()
                .get("path")
                .and_then(|p| p.as_str())
                .expect("Network namespace must have a path");
            assert_eq!(
                ns_path, "/run/netns/ssam-test",
                "Network namespace path must match supplied netns path"
            );
        }

        #[test]
        fn bridge_without_path_errors() {
            let oci_spec = create_test_oci_spec();
            let temp_dir = TempDir::new().expect("Failed to create temp dir");
            let mount_point = temp_dir.path().join("mount");
            fs::create_dir_all(&mount_point).expect("Failed to create mount point");

            let package_volume = create_test_package_volume_real(&mount_point)
                .expect("Failed to create test PackageVolume");

            let result = TransientRuntimeConfig::new(ContainerBundleSpec {
                oci_template: oci_spec,
                seccomp_policy: None,
                mac_enabled: false,
                network_mode: NetworkMode::Bridge,
                cgroups_path: "/sys/fs/cgroup/system.slice".to_owned(),
                package_name: "test-bridge-no-path".to_owned(),
                mount_point: package_volume.get_mount_point().to_path_buf(),
                data_mounts: None,
                bridge_netns_path: None,
            });

            assert!(result.is_err(), "Bridge without netns path must error");
            let err_msg = result.unwrap_err().to_string();
            assert!(
                err_msg.contains("bridge network mode requires a netns path"),
                "Error must describe missing netns path: {err_msg}"
            );
        }
    }
}
