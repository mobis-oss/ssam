// Copyright 2025-2026 Hyundai Mobis Co., Ltd.
// SPDX-License-Identifier: Apache-2.0

use std::sync::Arc;

pub(crate) mod oci_bundle_dir;

#[non_exhaustive]
#[derive(strum_macros::Display, Debug)]
pub(crate) enum ContainerRuntime {
    #[strum(serialize = "/usr/bin/crun")]
    CRun,
}

#[derive(Debug, Clone)] // TODO:: Get default executor from config
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
        packages_cgroup: String,
    ) -> anyhow::Result<Self> {
        let mgr = match execution_type {
            ExecutorType::Systemd(service_info) => systemd::TransientUnitExecutor::new(
                package_name,
                command,
                service_info,
                state_sender,
                packages_cgroup,
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
    use std::path::{Path, PathBuf};

    use super::oci_bundle_dir::OciBundleDir;
    use anyhow::Context;
    use libssam::container::NetworkMode;
    use libssam::ssam_package::PackageSeccompPolicy;
    use libssam::utils::PrettyJsonWriter;
    use oci_spec::runtime::{
        Arch, Linux, LinuxNamespace, LinuxNamespaceBuilder, LinuxNamespaceType, LinuxSeccomp,
        Mount, MountBuilder, RootBuilder,
    };
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
    pub(crate) struct OciRuntimeConfig;

    impl OciRuntimeConfig {
        const MOUNT_OPTION: [&'static str; 2] = ["rbind", "rw"];
        const CONTAINER_DEFAULT_APPARMOR_PROFILE: &str = "container-default";

        pub(crate) fn write(
            spec: ContainerBundleSpec,
            oci_bundle_dir: &OciBundleDir,
        ) -> anyhow::Result<()> {
            let ContainerBundleSpec {
                oci_template,
                seccomp_policy,
                mac_enabled,
                network_mode,
                cgroups_path,
                package_name,
                data_mounts,
                bridge_netns_path,
                ..
            } = spec;

            // Just absolutize the rootfs path. The rootfs might not be mounted yet.
            let rootfs_path =
                std::path::absolute(oci_bundle_dir.pkgfs_path()).with_context(|| {
                    format!("Unable to absoluteize rootfs directory of {package_name}")
                })?;
            let mut oci_runtime_conf = oci_template;

            let linux = oci_runtime_conf.linux_mut().get_or_insert(Linux::default());

            let seccomp =
                Self::build_seccomp_config(seccomp_policy.as_ref()).with_context(|| {
                    format!(
                        "{package_name}: refusing to start container with unmappable seccomp policy"
                    )
                })?;
            if seccomp.is_none() {
                log::info!("{package_name}: seccomp is disabled by configuration");
            }
            linux.set_seccomp(seccomp);

            let namespaces = linux.namespaces_mut().get_or_insert_with(Vec::new);
            Self::configure_network_namespace(
                namespaces,
                &network_mode,
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

            let new_conf_path = oci_bundle_dir.as_path().join("config.json");

            oci_runtime_conf
                .save_pretty(new_conf_path.as_path())
                .with_context(|| {
                    format!(
                        "Failed to save container runtime spec to {}",
                        new_conf_path.display()
                    )
                })?;

            Ok(())
        }

        // TODO: ssam-wrap will validate that runtime.json does not contain a
        // pre-configured network namespace when package config specifies network
        // mode. If both are present, ssam-wrap should reject the package at build
        // time rather than silently overriding here.
        // Safety: Debug format ({:?}) for the netns path instead of Display to
        // prevent log injection via special characters in the path.
        #[allow(clippy::use_debug, clippy::unnecessary_debug_formatting)]
        fn configure_network_namespace(
            namespaces: &mut Vec<LinuxNamespace>,
            network_mode: &NetworkMode,
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
                NetworkMode::Netns(path) => {
                    crate::network::netns::verify_is_netns(path)
                        .context("Failed to verify external netns")?;
                    let ns = LinuxNamespaceBuilder::default()
                        .typ(LinuxNamespaceType::Network)
                        .path(path.clone())
                        .build()
                        .context("Failed to build external netns entry")?;
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
        /// `Ok(None)` means no policy is present and seccomp is intentionally disabled.
        /// A present-but-unmappable policy is an error: the container must not run
        /// without the filter it was configured to have (fail-closed).
        ///
        /// # Errors
        ///
        /// Returns an error if a policy is present but cannot be mapped to the OCI
        /// `LinuxSeccomp` schema.
        fn build_seccomp_config(
            seccomp_policy: Option<&PackageSeccompPolicy>,
        ) -> anyhow::Result<Option<LinuxSeccomp>> {
            let Some(policy) = seccomp_policy else {
                return Ok(None);
            };

            let seccomp_json: serde_json::Value = serde_json::from_slice(policy.as_bytes())
                .context("Failed to parse seccomp profile JSON")?;

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

            let mut seccomp: LinuxSeccomp = serde_json::from_value(seccomp_json)
                .context("Failed to map seccomp policy to OCI LinuxSeccomp schema")?;
            seccomp.set_architectures(architectures);
            Ok(Some(seccomp))
        }

        fn make_bind_mounts(
            package_data_root: &Path,
            data_dirs: Vec<&Path>,
        ) -> anyhow::Result<Vec<Mount>> {
            let dest_prefix = Path::new("/");
            data_dirs
                .into_iter()
                .map(|dest_path| {
                    let relative = dest_path.strip_prefix("/").unwrap_or(dest_path);
                    anyhow::ensure!(
                        crate::utils::is_safe_relative_path(relative),
                        "Unsafe data_dir bind mount escapes root: {}",
                        dest_path.display()
                    );
                    let src_path = package_data_root.join(relative);
                    let dest = dest_prefix.join(relative);
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
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn build_seccomp_config_none_is_disabled() {
            let seccomp = OciRuntimeConfig::build_seccomp_config(None).unwrap();
            assert!(seccomp.is_none(), "no policy => intentionally disabled");
        }

        #[test]
        fn build_seccomp_config_valid_policy_is_applied() {
            let policy = PackageSeccompPolicy::from_bytes(
                br#"{"defaultAction":"SCMP_ACT_ERRNO","syscalls":[]}"#.to_vec(),
            )
            .unwrap();
            let seccomp = OciRuntimeConfig::build_seccomp_config(Some(&policy))
                .expect("valid policy must map")
                .expect("must produce a filter");
            assert_eq!(
                seccomp.default_action(),
                oci_spec::runtime::LinuxSeccompAction::ScmpActErrno
            );
        }

        #[test]
        fn build_seccomp_config_bad_oci_schema_fails_closed() {
            // Valid JSON, invalid OCI enum => must error, never silently disable.
            let policy =
                PackageSeccompPolicy::from_bytes(br#"{"defaultAction":"SCMP_ACT_BOGUS"}"#.to_vec())
                    .unwrap();
            assert!(OciRuntimeConfig::build_seccomp_config(Some(&policy)).is_err());
        }

        #[test]
        fn build_seccomp_config_missing_default_action_fails_closed() {
            let policy = PackageSeccompPolicy::from_bytes(br#"{"syscalls":[]}"#.to_vec()).unwrap();
            assert!(OciRuntimeConfig::build_seccomp_config(Some(&policy)).is_err());
        }

        #[test]
        fn test_make_bind_mounts_rejects_traversal_data_dir() {
            let package_data_root = Path::new("/package-data/test-package");

            let result = OciRuntimeConfig::make_bind_mounts(
                package_data_root,
                vec![Path::new("/../escape")],
            );

            assert!(result.is_err());
        }

        #[test]
        fn test_make_bind_mounts_uses_validated_relative_path() {
            let package_data_root = Path::new("/package-data/test-package");

            let mounts =
                OciRuntimeConfig::make_bind_mounts(package_data_root, vec![Path::new("/var/data")])
                    .unwrap();

            let mount_json = serde_json::to_value(&mounts[0]).unwrap();
            assert_eq!(mount_json["source"], "/package-data/test-package/var/data");
            assert_eq!(mount_json["destination"], "/var/data");
        }

        #[test]
        fn netns_arm_fails_for_non_netns_path() {
            let file = tempfile::NamedTempFile::new().expect("tempfile");
            let mut namespaces = Vec::new();
            let err = OciRuntimeConfig::configure_network_namespace(
                &mut namespaces,
                &NetworkMode::Netns(file.path().to_path_buf()),
                "test-netns-pkg",
                None,
            )
            .expect_err("a plain file is not a netns");
            assert!(
                format!("{err:#}").contains("external netns"),
                "unexpected error: {err:#}"
            );
        }

        // `configure_network_namespace` calls `verify_is_netns`, which requires a
        // real network namespace (root + CLONE_NEWNET) to succeed; run under the
        // Docker test harness instead of plain `cargo test`.
        #[test]
        #[ignore = "requires root and CLONE_NEWNET; run under the Docker test harness"]
        fn netns_arm_sets_path_when_verified() {
            let path = crate::network::netns::netns_path("ssam-executor-netns-test");
            let _ = crate::network::netns::delete_named_netns(&path);
            crate::network::netns::ensure_netns_tree_trusted().unwrap();
            crate::network::netns::create_named_netns(&path).unwrap();

            let mut namespaces = Vec::new();
            OciRuntimeConfig::configure_network_namespace(
                &mut namespaces,
                &NetworkMode::Netns(path.clone()),
                "test-netns-pkg",
                None,
            )
            .expect("a verified netns must succeed");

            let net_ns = namespaces
                .iter()
                .find(|ns| ns.typ() == LinuxNamespaceType::Network)
                .expect("must add a network namespace entry");
            assert_eq!(net_ns.path(), &Some(path.clone()));

            crate::network::netns::delete_named_netns(&path).unwrap();
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
        pub(crate) fn write_oci_runtime_config(
            self,
            oci_bundle_dir: &OciBundleDir,
        ) -> anyhow::Result<()> {
            OciRuntimeConfig::write(self, oci_bundle_dir)
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
    async fn prepare(&self) -> anyhow::Result<()> {
        Ok(())
    }
    fn get_start_cmd(&self) -> anyhow::Result<Vec<String>>;
    fn get_stop_cmd(&self) -> anyhow::Result<Vec<String>>;
}

pub(crate) mod container {
    use std::sync::Arc;

    use crate::executor::oci_bundle_dir::OciBundleDir;
    use crate::network::netns;
    use crate::package_volume::PackageVolume;
    use anyhow::Context as _;
    use libssam::container;
    use libssam::ssam_package::PackageFile;

    use super::{ContainerRuntime, ExecuteCommand, oci};

    #[derive(Debug)]
    pub(crate) struct ContainerCommand {
        name: String,
        runtime: ContainerRuntime,
        spec: oci::ContainerBundleSpec,
        oci_bundle_dir: Arc<OciBundleDir>,
    }

    impl ContainerCommand {
        pub(crate) fn new(
            name: String,
            runtime: ContainerRuntime,
            spec: oci::ContainerBundleSpec,
            oci_bundle_dir: Arc<OciBundleDir>,
        ) -> Self {
            Self {
                name,
                runtime,
                spec,
                oci_bundle_dir,
            }
        }

        /// Constructs a bundle specification from package metadata and volume.
        /// Extracts OCI config, security settings, network mode, and data paths.
        pub(crate) fn spec_from_package(
            name: &str,
            package_file: &PackageFile,
            pkg_volume: &PackageVolume,
            network_mode: container::NetworkMode,
            packages_cgroup: &str,
        ) -> oci::ContainerBundleSpec {
            let metadata = package_file.metadata();

            let seccomp_policy = metadata
                .get_container_security_seccomp()
                .then(|| package_file.seccomp_policy().clone());

            let cgroups_path = super::systemd::cgroups_path(packages_cgroup).to_owned();

            let data_mounts = pkg_volume.data_directory().map(oci::DataMountPaths::new);

            let bridge_netns_path = if network_mode == container::NetworkMode::Bridge {
                Some(netns::netns_path(&netns::netns_name(name)))
            } else {
                None
            };

            // Deref coercion: &OciRuntimeConfig → &Box<Spec> → &Spec
            let oci_runtime_spec: &oci_spec::runtime::Spec = package_file.oci_runtime_config();

            oci::ContainerBundleSpec {
                oci_template: oci_runtime_spec.clone(),
                seccomp_policy,
                mac_enabled: *metadata.get_container_security_mac(),
                network_mode,
                cgroups_path,
                package_name: name.to_owned(),
                data_mounts,
                bridge_netns_path,
            }
        }

        pub(crate) fn from_package(
            name: &str,
            package_file: &PackageFile,
            pkg_volume: &PackageVolume,
            network_mode: container::NetworkMode,
            packages_cgroup: &str,
            oci_bundle_dir: Arc<OciBundleDir>,
        ) -> Arc<dyn ExecuteCommand> {
            let spec = Self::spec_from_package(
                name,
                package_file,
                pkg_volume,
                network_mode,
                packages_cgroup,
            );
            Arc::new(Self::new(
                name.to_owned(),
                ContainerRuntime::CRun,
                spec,
                oci_bundle_dir,
            ))
        }
    }

    #[async_trait::async_trait]
    impl ExecuteCommand for ContainerCommand {
        async fn prepare(&self) -> anyhow::Result<()> {
            let spec = self.spec.clone();
            let oci_bundle_dir = Arc::clone(&self.oci_bundle_dir);
            tokio::task::spawn_blocking(move || spec.write_oci_runtime_config(&oci_bundle_dir))
                .await
                .context("join container bundle preparation task")?
        }

        fn get_start_cmd(&self) -> anyhow::Result<Vec<String>> {
            let path = self.oci_bundle_dir.as_path();
            if !path.exists() {
                anyhow::bail!("Bundle path does not exist: {}", path.display());
            }
            if !self.oci_bundle_dir.as_path().join("config.json").is_file() {
                anyhow::bail!("OCI runtime config not prepared");
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
    use super::oci::{ContainerBundleSpec, DataMountPaths, OciRuntimeConfig};
    use crate::executor::oci_bundle_dir::OciBundleDir;
    use crate::package_volume::{DataDirectory, PackageVolume};
    use libssam::ssam_package::PackageSeccompPolicy;
    use oci_spec::runtime::{LinuxBuilder, ProcessBuilder, RootBuilder, Spec};
    use std::fs;
    use std::path::{Path, PathBuf};
    use tempfile::TempDir;

    #[derive(Debug)]
    struct TestOciRuntimeConfig {
        oci_bundle_dir: OciBundleDir,
    }

    impl TestOciRuntimeConfig {
        fn new(
            oci_bundle_root: &Path,
            version: &str,
            spec: ContainerBundleSpec,
        ) -> anyhow::Result<Self> {
            let oci_bundle_dir = OciBundleDir::new(oci_bundle_root, &spec.package_name, version)?;
            oci_bundle_dir.ensure()?;
            OciRuntimeConfig::write(spec, &oci_bundle_dir)?;
            Ok(Self { oci_bundle_dir })
        }

        fn dir_path(&self) -> &Path {
            self.oci_bundle_dir.as_path()
        }

        fn cleanup_bundle(&self) -> anyhow::Result<()> {
            self.oci_bundle_dir.cleanup()
        }
    }

    // Helper to create a test seccomp policy
    fn create_test_seccomp_policy() -> PackageSeccompPolicy {
        PackageSeccompPolicy::default_policy()
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
        let pkgfs_meta = PackageFsMetadata::new_for_test(mount_point, &test_pkg_file)?;

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
    fn test_oci_runtime_config_creation() {
        let oci_spec = create_test_oci_spec();
        let seccomp_policy = create_test_seccomp_policy();
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let mount_point = temp_dir.path().join("mount");
        fs::create_dir_all(&mount_point).expect("Failed to create mount point");

        let result = TestOciRuntimeConfig::new(
            &mount_point,
            "1.0.0",
            ContainerBundleSpec {
                oci_template: oci_spec,
                seccomp_policy: Some(seccomp_policy),
                mac_enabled: true,
                network_mode: libssam::container::NetworkMode::None,
                cgroups_path: "/sys/fs/cgroup/system.slice".to_owned(),
                package_name: "test-package-basic".to_owned(),
                data_mounts: None,
                bridge_netns_path: None,
            },
        );

        assert!(
            result.is_ok(),
            "Test runtime config creation should succeed: {:?}",
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
            content.contains("test-package-basic"),
            "Should contain package name as hostname"
        );
        assert!(
            content.contains("/sys/fs/cgroup/system.slice/test-package-basic.service/container"),
            "Should contain correct cgroups path"
        );
    }

    #[test]
    fn test_oci_runtime_config_bundle_dir_is_0755() {
        use std::os::unix::fs::PermissionsExt;

        let oci_spec = create_test_oci_spec();
        let seccomp_policy = create_test_seccomp_policy();
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let mount_point = temp_dir.path().join("mount");
        fs::create_dir_all(&mount_point).expect("Failed to create mount point");

        let config = TestOciRuntimeConfig::new(
            &mount_point,
            "1.0.0",
            ContainerBundleSpec {
                oci_template: oci_spec,
                seccomp_policy: Some(seccomp_policy),
                mac_enabled: true,
                network_mode: libssam::container::NetworkMode::None,
                cgroups_path: "/sys/fs/cgroup/system.slice".to_owned(),
                package_name: "test-package-mode".to_owned(),
                data_mounts: None,
                bridge_netns_path: None,
            },
        )
        .expect("Test runtime config creation should succeed");

        let meta = fs::symlink_metadata(config.dir_path()).expect("Failed to stat bundle dir");
        assert_eq!(
            meta.permissions().mode() & 0o777,
            0o755,
            "Bundle directory must be 0755 so a separate-uid runtime consumer can read it"
        );
    }

    #[test]
    fn test_oci_runtime_config_uses_fixed_package_dir() {
        let oci_spec = create_test_oci_spec();
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let mount_point = temp_dir.path().join("mount");
        fs::create_dir_all(&mount_point).expect("Failed to create mount point");

        let config = TestOciRuntimeConfig::new(
            &mount_point,
            "1.0.0",
            ContainerBundleSpec {
                oci_template: oci_spec,
                seccomp_policy: None,
                mac_enabled: false,
                network_mode: libssam::container::NetworkMode::None,
                cgroups_path: "/sys/fs/cgroup/system.slice".to_owned(),
                package_name: "test-package-fixed-dir".to_owned(),
                data_mounts: None,
                bridge_netns_path: None,
            },
        )
        .expect("Test runtime config creation should succeed");

        assert_eq!(
            config.dir_path(),
            mount_point.join("test-package-fixed-dir_1.0.0")
        );
        assert!(
            config.dir_path().join("config.json").exists(),
            "config.json should be created in the fixed package directory"
        );

        let bundle_path = config.dir_path().to_path_buf();
        let config_path = config.dir_path().join("config.json");
        drop(config);
        assert!(
            config_path.exists(),
            "config.json should remain until runtime cleanup"
        );
        crate::utils::remove_path_if_exists(&bundle_path).expect("bundle cleanup should succeed");
        assert!(
            !bundle_path.exists(),
            "runtime cleanup should remove the bundle directory"
        );
    }

    #[test]
    fn test_oci_runtime_config_removes_existing_bundle_dir() {
        let package_name = "test-package-stale-dir";
        let oci_spec = create_test_oci_spec();
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let mount_point = temp_dir.path().join("mount");
        fs::create_dir_all(&mount_point).expect("Failed to create mount point");
        let bundle_path = mount_point.join(format!("{package_name}_1.0.0"));
        fs::create_dir_all(&bundle_path).expect("Failed to create stale bundle dir");
        let stale_file = bundle_path.join("stale");
        fs::write(&stale_file, "stale").expect("Failed to write stale file");

        let config = TestOciRuntimeConfig::new(
            &mount_point,
            "1.0.0",
            ContainerBundleSpec {
                oci_template: oci_spec,
                seccomp_policy: None,
                mac_enabled: false,
                network_mode: libssam::container::NetworkMode::None,
                cgroups_path: "/sys/fs/cgroup/system.slice".to_owned(),
                package_name: package_name.to_owned(),
                data_mounts: None,
                bridge_netns_path: None,
            },
        )
        .expect("Test runtime config creation should succeed");

        assert_eq!(config.dir_path(), bundle_path);
        assert!(
            !stale_file.exists(),
            "existing bundle dir should be removed before recreation"
        );
        assert!(
            config.dir_path().join("config.json").exists(),
            "config.json should be created after recreating bundle dir"
        );
    }

    #[test]
    fn test_oci_runtime_config_cleanup_removes_package_dir() {
        let package_name = "test-package-drop-cleanup";
        let oci_spec = create_test_oci_spec();
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let mount_point = temp_dir.path().join("mount");
        fs::create_dir_all(&mount_point).expect("Failed to create mount point");

        let config = TestOciRuntimeConfig::new(
            &mount_point,
            "1.0.0",
            ContainerBundleSpec {
                oci_template: oci_spec,
                seccomp_policy: None,
                mac_enabled: false,
                network_mode: libssam::container::NetworkMode::None,
                cgroups_path: "/sys/fs/cgroup/system.slice".to_owned(),
                package_name: package_name.to_owned(),
                data_mounts: None,
                bridge_netns_path: None,
            },
        )
        .expect("Test runtime config creation should succeed");

        let bundle_path = config.dir_path().to_path_buf();
        assert!(bundle_path.exists(), "bundle dir should exist before drop");

        config
            .cleanup_bundle()
            .expect("bundle directory cleanup should succeed");
        drop(config);

        assert!(
            !bundle_path.exists(),
            "bundle dir should be removed by explicit cleanup"
        );
    }

    #[test]
    fn test_oci_runtime_config_distinct_versions_isolated() {
        let package_name = "test-package-recreated";
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let mount_point = temp_dir.path().join("mount");
        fs::create_dir_all(&mount_point).expect("Failed to create mount point");

        let old_config = TestOciRuntimeConfig::new(
            &mount_point,
            "1.0.0",
            ContainerBundleSpec {
                oci_template: create_test_oci_spec(),
                seccomp_policy: None,
                mac_enabled: false,
                network_mode: libssam::container::NetworkMode::None,
                cgroups_path: "/sys/fs/cgroup/system.slice".to_owned(),
                package_name: package_name.to_owned(),
                data_mounts: None,
                bridge_netns_path: None,
            },
        )
        .expect("old runtime config should be created");
        let old_path = old_config.dir_path().to_path_buf();

        let new_config = TestOciRuntimeConfig::new(
            &mount_point,
            "2.0.0",
            ContainerBundleSpec {
                oci_template: create_test_oci_spec(),
                seccomp_policy: None,
                mac_enabled: false,
                network_mode: libssam::container::NetworkMode::None,
                cgroups_path: "/sys/fs/cgroup/system.slice".to_owned(),
                package_name: package_name.to_owned(),
                data_mounts: None,
                bridge_netns_path: None,
            },
        )
        .expect("new runtime config should be created");
        let new_path = new_config.dir_path().to_path_buf();

        // The version keeps an upgrade's two live configs on distinct paths, so
        // cleaning the superseded one cannot touch the incoming bundle.
        assert_ne!(old_path, new_path);

        old_config
            .cleanup_bundle()
            .expect("old bundle directory cleanup should succeed");
        drop(old_config);

        assert!(
            new_path.join("config.json").exists(),
            "cleaning the old version must not remove the new version's bundle"
        );
        assert!(!old_path.exists(), "the old version's bundle is cleaned up");
    }

    #[test]
    fn test_oci_runtime_config_with_data_directory() {
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

        let result = TestOciRuntimeConfig::new(
            &mount_point,
            "1.0.0",
            ContainerBundleSpec {
                oci_template: oci_spec,
                seccomp_policy: Some(seccomp_policy),
                mac_enabled: true,
                network_mode: libssam::container::NetworkMode::None,
                cgroups_path: "/sys/fs/cgroup/system.slice".to_owned(),
                package_name: "test-package-data".to_owned(),
                data_mounts,
                bridge_netns_path: None,
            },
        );

        assert!(
            result.is_ok(),
            "Test runtime config creation with data dirs should succeed: {:?}",
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
    fn test_oci_runtime_config_seccomp_disabled() {
        // Test that seccomp is not applied when seccomp policy is None
        let oci_spec = create_test_oci_spec();
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let mount_point = temp_dir.path().join("mount");
        fs::create_dir_all(&mount_point).expect("Failed to create mount point");

        let result = TestOciRuntimeConfig::new(
            &mount_point,
            "1.0.0",
            ContainerBundleSpec {
                oci_template: oci_spec,
                seccomp_policy: None,
                mac_enabled: true,
                network_mode: libssam::container::NetworkMode::None,
                cgroups_path: "/sys/fs/cgroup/system.slice".to_owned(),
                package_name: "test-package-no-seccomp".to_owned(),
                data_mounts: None,
                bridge_netns_path: None,
            },
        );

        assert!(
            result.is_ok(),
            "Test runtime config creation with seccomp disabled should succeed: {:?}",
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
    fn test_oci_runtime_config_seccomp() {
        // Test that seccomp is applied when seccomp policy is provided
        let oci_spec = create_test_oci_spec();
        let seccomp_policy = create_test_seccomp_policy();
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let mount_point = temp_dir.path().join("mount");
        fs::create_dir_all(&mount_point).expect("Failed to create mount point");

        let result = TestOciRuntimeConfig::new(
            &mount_point,
            "1.0.0",
            ContainerBundleSpec {
                oci_template: oci_spec,
                seccomp_policy: Some(seccomp_policy),
                mac_enabled: true,
                network_mode: libssam::container::NetworkMode::None,
                cgroups_path: "/sys/fs/cgroup/system.slice".to_owned(),
                package_name: "test-package-with-seccomp".to_owned(),
                data_mounts: None,
                bridge_netns_path: None,
            },
        );

        assert!(
            result.is_ok(),
            "Test runtime config creation with seccomp enabled should succeed: {:?}",
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

    #[test]
    fn test_oci_runtime_config_seccomp_mapping_failure_fails_closed() {
        let oci_spec = create_test_oci_spec();
        let bad_policy =
            PackageSeccompPolicy::from_bytes(br#"{"defaultAction":"SCMP_ACT_BOGUS"}"#.to_vec())
                .expect("valid JSON, invalid OCI schema");
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let mount_point = temp_dir.path().join("mount");
        fs::create_dir_all(&mount_point).expect("Failed to create mount point");

        let result = TestOciRuntimeConfig::new(
            &mount_point,
            "1.0.0",
            ContainerBundleSpec {
                oci_template: oci_spec,
                seccomp_policy: Some(bad_policy),
                mac_enabled: true,
                network_mode: libssam::container::NetworkMode::None,
                cgroups_path: "/sys/fs/cgroup/system.slice".to_owned(),
                package_name: "test-package-bad-seccomp".to_owned(),
                data_mounts: None,
                bridge_netns_path: None,
            },
        );

        assert!(
            result.is_err(),
            "unmappable seccomp policy must refuse to build bundle"
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
        use std::sync::Arc;

        use super::*;
        use crate::executor::container::ContainerCommand;
        use crate::executor::oci_bundle_dir::OciBundleDir;
        use crate::executor::{ContainerRuntime, ExecuteCommand};

        fn create_test_container_command(
            package_name: &str,
        ) -> (ContainerCommand, Arc<OciBundleDir>, TempDir) {
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
                package_name: package_name.to_owned(),
                data_mounts: None,
                bridge_netns_path: None,
            };
            let oci_bundle_dir = OciBundleDir::new(&mount_point, package_name, "1.0.0")
                .expect("test bundle path should be valid");
            oci_bundle_dir
                .ensure()
                .expect("test bundle directory should be created");
            let oci_bundle_dir = Arc::new(oci_bundle_dir);
            (
                ContainerCommand::new(
                    package_name.to_owned(),
                    ContainerRuntime::CRun,
                    spec,
                    Arc::clone(&oci_bundle_dir),
                ),
                oci_bundle_dir,
                temp_dir,
            )
        }

        #[test]
        fn test_container_command_new() {
            let (executor, _bundle_path, _temp_dir) =
                create_test_container_command("test-container-new");

            // Test that the ContainerExecutor was created successfully
            assert!(format!("{executor:?}").contains("test-container-new"));
            assert!(format!("{executor:?}").contains("CRun"));
        }

        #[tokio::test]
        async fn test_container_command_get_start_args() {
            let (executor, _bundle_path, _temp_dir) =
                create_test_container_command("test-container-start-args");

            executor
                .prepare()
                .await
                .expect("command prepare should succeed");

            let start_args = executor.get_start_cmd().expect("Should get start command");

            assert_eq!(start_args.len(), 5);
            assert_eq!(start_args[0], "/usr/bin/crun");
            assert_eq!(start_args[1], "run");
            assert_eq!(start_args[2], "--bundle");
            assert_eq!(start_args[4], "test-container-start-args");

            let bundle_path = Path::new(&start_args[3]);
            assert!(
                bundle_path.exists(),
                "Bundle path should exist: {bundle_path:?}"
            );
        }

        #[test]
        fn test_container_command_get_stop_args() {
            let (executor, _bundle_path, _temp_dir) =
                create_test_container_command("test-container-stop-args");

            let stop_args = executor.get_stop_cmd().expect("Should get stop command");

            // Verify the stop command structure
            assert_eq!(stop_args.len(), 4);
            assert_eq!(stop_args[0], "/usr/bin/crun");
            assert_eq!(stop_args[1], "delete");
            assert_eq!(stop_args[2], "--force");
            assert_eq!(stop_args[3], "test-container-stop-args");
        }

        #[tokio::test]
        async fn test_container_command_get_start_args_bundle_not_exists() {
            let (executor, bundle_path, _temp_dir) =
                create_test_container_command("test-container-missing-bundle");

            executor
                .prepare()
                .await
                .expect("command prepare should succeed");

            fs::remove_dir_all(bundle_path.as_path()).expect("Failed to remove bundle dir");

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
            let (executor, _bundle_path, _temp_dir) =
                create_test_container_command("test-container-unprepared");

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
            let (executor, bundle_path, _temp_dir) =
                create_test_container_command("test-container-idempotent");

            executor
                .prepare()
                .await
                .expect("first prepare should succeed");
            let first_path = bundle_path.as_path().to_path_buf();

            executor
                .prepare()
                .await
                .expect("second prepare should succeed");
            let second_path = bundle_path.as_path().to_path_buf();

            assert_eq!(first_path, second_path);
        }

        #[tokio::test]
        async fn test_container_command_prepares_bundle_before_mount_target() {
            let (executor, bundle_path, _temp_dir) =
                create_test_container_command("test-container-mount-order");

            assert!(
                bundle_path.as_path().exists(),
                "bundle directory should exist"
            );
            assert!(
                !bundle_path.as_path().join("config.json").exists(),
                "bundle config should be written by command preparation"
            );

            fs::create_dir(bundle_path.pkgfs_path()).expect("mount target should be creatable");
            executor
                .prepare()
                .await
                .expect("command preparation should reuse pre-created bundle");

            assert!(bundle_path.pkgfs_path().exists());
            assert!(bundle_path.as_path().join("config.json").exists());
            let start_args = executor
                .get_start_cmd()
                .expect("prepared bundle should produce start arguments");
            assert_eq!(Path::new(&start_args[3]), bundle_path.as_path());
        }

        #[tokio::test]
        async fn test_container_command_prepared_get_start_args_valid() {
            let (executor, _bundle_path, _temp_dir) =
                create_test_container_command("test-container-prepared");

            executor
                .prepare()
                .await
                .expect("command prepare should succeed");
            let start_args = executor.get_start_cmd().expect("Should get start command");

            assert_eq!(start_args[0], "/usr/bin/crun");
            assert_eq!(start_args[1], "run");
            assert_eq!(start_args[2], "--bundle");
            assert!(Path::new(&start_args[3]).exists());
            assert_eq!(start_args[4], "test-container-prepared");
        }

        #[test]
        fn test_container_runtime_display() {
            let runtime = ContainerRuntime::CRun;
            assert_eq!(runtime.to_string(), "/usr/bin/crun");
        }

        #[test]
        fn test_container_command_debug_format() {
            let (executor, _bundle_path, _temp_dir) =
                create_test_container_command("test-container-debug");
            let debug_str = format!("{executor:?}");

            // Verify debug format contains expected information
            assert!(debug_str.contains("ContainerCommand"));
            assert!(debug_str.contains("test-container-debug"));
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
        use crate::executor::oci::{HostArch, OciRuntimeConfig};
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
            let result =
                OciRuntimeConfig::convert_arch_map_to_architectures(arch_map_arr, HostArch::X86_64);

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
            let result = OciRuntimeConfig::convert_arch_map_to_architectures(
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
            let result =
                OciRuntimeConfig::convert_arch_map_to_architectures(arch_map_arr, HostArch::X86);

            assert!(result.is_some());
            let archs = result.unwrap();
            assert_eq!(archs.len(), 1);
            assert_eq!(archs[0], Arch::ScmpArchX86);
        }

        #[test]
        fn test_convert_arch_map_arm() {
            let arch_map = create_test_arch_map();
            let arch_map_arr = arch_map.as_array().unwrap();
            let result =
                OciRuntimeConfig::convert_arch_map_to_architectures(arch_map_arr, HostArch::Arm);

            assert!(result.is_some());
            let archs = result.unwrap();
            assert_eq!(archs.len(), 1);
            assert_eq!(archs[0], Arch::ScmpArchArm);
        }

        #[test]
        fn test_convert_arch_map_unsupported() {
            let arch_map = create_test_arch_map();
            let arch_map_arr = arch_map.as_array().unwrap();
            let result = OciRuntimeConfig::convert_arch_map_to_architectures(
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

            let result =
                OciRuntimeConfig::convert_arch_map_to_architectures(arch_map_arr, HostArch::X86_64);
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

            let result =
                OciRuntimeConfig::convert_arch_map_to_architectures(arch_map_arr, HostArch::X86_64);
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

            let result =
                OciRuntimeConfig::convert_arch_map_to_architectures(arch_map_arr, HostArch::X86_64);
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

            let result =
                OciRuntimeConfig::convert_arch_map_to_architectures(arch_map_arr, HostArch::X86_64);
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

            let result =
                OciRuntimeConfig::convert_arch_map_to_architectures(&arch_map, HostArch::X86_64);
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

            let result = TestOciRuntimeConfig::new(
                &mount_point,
                "1.0.0",
                ContainerBundleSpec {
                    oci_template: oci_spec,
                    seccomp_policy: None,
                    mac_enabled: false,
                    network_mode: NetworkMode::Host,
                    cgroups_path: "/sys/fs/cgroup/system.slice".to_owned(),
                    package_name: "test-host-net".to_owned(),
                    data_mounts: None,
                    bridge_netns_path: None,
                },
            );

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

            let result = TestOciRuntimeConfig::new(
                &mount_point,
                "1.0.0",
                ContainerBundleSpec {
                    oci_template: oci_spec,
                    seccomp_policy: None,
                    mac_enabled: false,
                    network_mode: NetworkMode::None,
                    cgroups_path: "/sys/fs/cgroup/system.slice".to_owned(),
                    package_name: "test-none-net".to_owned(),
                    data_mounts: None,
                    bridge_netns_path: None,
                },
            );

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

            let result = TestOciRuntimeConfig::new(
                &mount_point,
                "1.0.0",
                ContainerBundleSpec {
                    oci_template: oci_spec,
                    seccomp_policy: None,
                    mac_enabled: false,
                    network_mode: NetworkMode::None,
                    cgroups_path: "/sys/fs/cgroup/system.slice".to_owned(),
                    package_name: "test-none-dup".to_owned(),
                    data_mounts: None,
                    bridge_netns_path: None,
                },
            );

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

            let netns_path = std::path::Path::new("/run/netns/ssam-test");
            let result = TestOciRuntimeConfig::new(
                &mount_point,
                "1.0.0",
                ContainerBundleSpec {
                    oci_template: oci_spec,
                    seccomp_policy: None,
                    mac_enabled: false,
                    network_mode: NetworkMode::Bridge,
                    cgroups_path: "/sys/fs/cgroup/system.slice".to_owned(),
                    package_name: "test-bridge-path".to_owned(),
                    data_mounts: None,
                    bridge_netns_path: Some(netns_path.to_path_buf()),
                },
            );

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

            let result = TestOciRuntimeConfig::new(
                &mount_point,
                "1.0.0",
                ContainerBundleSpec {
                    oci_template: oci_spec,
                    seccomp_policy: None,
                    mac_enabled: false,
                    network_mode: NetworkMode::Bridge,
                    cgroups_path: "/sys/fs/cgroup/system.slice".to_owned(),
                    package_name: "test-bridge-no-path".to_owned(),
                    data_mounts: None,
                    bridge_netns_path: None,
                },
            );

            assert!(result.is_err(), "Bridge without netns path must error");
            let err_msg = result.unwrap_err().to_string();
            assert!(
                err_msg.contains("bridge network mode requires a netns path"),
                "Error must describe missing netns path: {err_msg}"
            );
        }
    }
}
