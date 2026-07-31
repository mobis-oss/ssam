// Copyright 2025-2026 Hyundai Mobis Co., Ltd.
// SPDX-License-Identifier: Apache-2.0

use anyhow::Context;
use libssam::utils::PrettyJsonWriter;
use oci_spec::runtime::{LinuxNamespaceType, ProcessBuilder};
use std::path::Path;

static OCI_IMAGE_TAG: &str = "ssam";
static OCI_IMAGE_DIRNAME: &str = "oci_unpack";
static OCI_UNPACK_DIRNAME: &str = "oci_unpack";

fn copy_runtime_config(src: impl AsRef<Path>, dest: impl AsRef<Path>) -> anyhow::Result<()> {
    let mut oci_runtime_conf =
        oci_spec::runtime::Spec::load(src).context("Failed to load container runtime spec")?;

    if let Some(p) = oci_runtime_conf.process_mut() {
        p.set_terminal(Some(false));
    } else {
        let process = ProcessBuilder::default().terminal(false).build()?;
        oci_runtime_conf.set_process(Some(process));
    }

    if let Some(linux) = oci_runtime_conf.linux_mut() {
        linux.set_uid_mappings(None);
        linux.set_gid_mappings(None);
        if let Some(namespaces) = linux.namespaces_mut() {
            namespaces.retain(|e| e.typ() != LinuxNamespaceType::User);
        }
    }

    let dest = dest.as_ref();
    oci_runtime_conf.save_pretty(dest).context(format!(
        "Failed to save container runtime spec to {}",
        dest.display()
    ))?;

    Ok(())
}

// Move the extracted rootfs to workspace.pkgfs, handling cross-partition moves
// Try to move with std::fs::rename first
fn move_dir(src: &Path, dest: &Path) -> anyhow::Result<()> {
    // Try to move with std::fs::rename first
    if let Err(e) = std::fs::rename(src, dest) {
        // If rename fails due to cross-device link, fallback to fs_extra copy
        if let Some(os_err) = e.raw_os_error() {
            // EXDEV (cross-device link) is errno 18, which always fits i32.
            #[allow(clippy::cast_possible_wrap)]
            let exdev = linux_raw_sys::errno::EXDEV as i32;
            if os_err == exdev {
                let mut options = fs_extra::dir::CopyOptions::new();
                options.overwrite = true;
                options.copy_inside = true;

                fs_extra::dir::copy(src, dest, &options).context(format!(
                    "Failed to copy rootfs from {} to {}",
                    src.display(),
                    dest.display()
                ))?;

                std::fs::remove_dir_all(src)
                    .context(format!("Failed to remove old rootfs at {}", src.display()))?;
            } else {
                return Err(e).context("Failed to rename rootfs directory");
            }
        } else {
            return Err(e).context("Failed to rename rootfs directory");
        }
    }

    Ok(())
}

pub fn extract_rootfs(
    workspace: &crate::Workspace,
    docker_uri: &str,
    architecture: super::OciArchitecture,
) -> anyhow::Result<()> {
    let runner = workspace.runner.as_ref();
    workspace.prepare_intermediate_dir()?;

    let oci_image_dir = workspace.intermediate_dir.join(OCI_IMAGE_DIRNAME);
    let oci_unpack_dir = workspace.intermediate_dir.join(OCI_UNPACK_DIRNAME);

    if workspace.pkgfs.exists() {
        anyhow::bail!(
            "Package filesystem already exists, please delete {}",
            workspace.pkgfs.display()
        );
    }

    if oci_image_dir.exists() {
        anyhow::bail!(
            "OCI image already exists, please delete {}",
            oci_image_dir.display()
        );
    }

    if oci_unpack_dir.exists() {
        anyhow::bail!(
            "OCI unpack directory already exists, please delete {}",
            oci_unpack_dir.display()
        );
    }

    let oci_image_name = format!(
        "{}:{OCI_IMAGE_TAG}",
        oci_image_dir
            .to_str()
            .context("Failed to convert OCI image path to string")?
    );

    if docker_uri.starts_with("docker-daemon:") {
        // Remove the prefix to get the actual image name
        let docker_uri = docker_uri.trim_start_matches("docker-daemon:");
        println!("Using Docker daemon URI: {docker_uri}");

        if let Ok(arch) = runner.execute_command(
            "docker",
            &["inspect", "--format='{{.Architecture}}'", docker_uri],
            true,
        ) {
            // Parse the architecture out of `docker inspect --format='{{.Architecture}}'`
            // output since some Docker CLI setups wrap the value in quotes.
            let arch = arch.trim_matches(|c: char| c == '\'' || c.is_whitespace());
            if arch != architecture.to_string() {
                return Err(anyhow::anyhow!(
                    "Docker image {docker_uri} is not for {architecture} architecture, found: {arch}",
                ));
            }
        } else {
            return Err(anyhow::anyhow!(
                "Failed to inspect Docker image {docker_uri}"
            ));
        }
    }

    runner.execute_command(
        "skopeo",
        &[
            "--insecure-policy",
            "copy",
            &format!("--override-arch={architecture}"),
            docker_uri,
            format!("oci:{oci_image_name}").as_str(),
        ],
        false,
    )?;

    runner.execute_command(
        "umoci",
        &[
            "unpack",
            "--rootless",
            "--image",
            oci_image_name.as_str(),
            oci_unpack_dir
                .to_str()
                .context("Failed to convert OCI unpack path to string")?,
        ],
        false,
    )?;

    if !workspace.runtime_config.exists() {
        println!("No runtime config provided, using unpacked one");
        copy_runtime_config(
            oci_unpack_dir.join("config.json"),
            &workspace.runtime_config,
        )?;
    }

    let oci_rootfs_path = oci_unpack_dir.join("rootfs");

    move_dir(&oci_rootfs_path, &workspace.pkgfs)?;

    println!("Successfully extracted {docker_uri}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{copy_runtime_config, extract_rootfs, move_dir};
    use crate::command::testing::{MockCommandRunner, MockResponse};
    use crate::pkgfs::OciArchitecture;
    use libssam::utils::PrettyJsonWriter;
    use oci_spec::runtime::{
        LinuxBuilder, LinuxIdMappingBuilder, LinuxNamespaceBuilder, LinuxNamespaceType,
        ProcessBuilder, RootBuilder, Spec, SpecBuilder,
    };
    use std::fs;
    use std::path::Path;
    use tempfile::tempdir;

    /// Write an OCI runtime spec fixture with terminal=true, uid/gid mappings,
    /// and both a User and a Pid namespace — i.e. everything copy_runtime_config
    /// is supposed to normalize away (except the Pid namespace, which stays).
    fn write_spec_fixture(path: &Path) {
        let process = ProcessBuilder::default().terminal(true).build().unwrap();
        let root = RootBuilder::default().path("/").build().unwrap();
        let id_map = LinuxIdMappingBuilder::default()
            .host_id(1000u32)
            .container_id(0u32)
            .size(1u32)
            .build()
            .unwrap();
        let namespaces = vec![
            LinuxNamespaceBuilder::default()
                .typ(LinuxNamespaceType::User)
                .build()
                .unwrap(),
            LinuxNamespaceBuilder::default()
                .typ(LinuxNamespaceType::Pid)
                .build()
                .unwrap(),
        ];
        let linux = LinuxBuilder::default()
            .uid_mappings(vec![id_map])
            .gid_mappings(vec![id_map])
            .namespaces(namespaces)
            .build()
            .unwrap();
        let spec = SpecBuilder::default()
            .version("1.0.2")
            .process(process)
            .root(root)
            .linux(linux)
            .build()
            .unwrap();
        spec.save_pretty(path).unwrap();
    }

    #[test]
    fn copy_runtime_config_strips_terminal_idmaps_and_user_ns() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("config.json");
        let dest = dir.path().join("runtime.json");
        write_spec_fixture(&src);

        copy_runtime_config(&src, &dest).unwrap();

        let out = Spec::load(&dest).unwrap();
        assert_eq!(out.process().as_ref().unwrap().terminal(), Some(false));
        let linux = out.linux().as_ref().unwrap();
        assert!(linux.uid_mappings().is_none());
        assert!(linux.gid_mappings().is_none());
        let ns = linux.namespaces().as_ref().unwrap();
        assert!(ns.iter().all(|n| n.typ() != LinuxNamespaceType::User));
        // Non-user namespaces are preserved.
        assert!(ns.iter().any(|n| n.typ() == LinuxNamespaceType::Pid));
    }

    #[test]
    fn move_dir_renames_within_partition() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("src");
        fs::create_dir(&src).unwrap();
        fs::write(src.join("f.txt"), b"hi").unwrap();
        let dest = dir.path().join("dest");

        move_dir(&src, &dest).unwrap();

        assert!(!src.exists());
        assert_eq!(fs::read(dest.join("f.txt")).unwrap(), b"hi");
        // The EXDEV (cross-device) copy fallback isn't reproducible in a unit
        // test since tempdir is always on a single filesystem.
    }

    #[test]
    fn extract_rootfs_bails_when_pkgfs_exists() {
        let dir = tempdir().unwrap();
        let mock = MockCommandRunner::new();
        let handle = mock.clone();
        let ws = crate::Workspace::new(dir.path(), Box::new(mock));
        fs::create_dir(&ws.pkgfs).unwrap();

        let res = extract_rootfs(&ws, "docker://busybox", OciArchitecture::Arm64);

        assert!(res.is_err());
        assert!(handle.calls().is_empty());
    }

    #[test]
    fn extract_rootfs_errors_on_arch_mismatch() {
        let dir = tempdir().unwrap();
        let mock = MockCommandRunner::with_responses(vec![MockResponse::stdout("amd64")]);
        let handle = mock.clone();
        let ws = crate::Workspace::new(dir.path(), Box::new(mock));

        let res = extract_rootfs(&ws, "docker-daemon:img", OciArchitecture::Arm64);

        assert!(res.is_err());
        let calls = handle.calls();
        assert_eq!(calls[0].cmd, "docker");
        // Arch check fails before the copy, so skopeo is never invoked.
        assert!(!calls.iter().any(|c| c.cmd == "skopeo"));
    }

    #[test]
    fn extract_rootfs_normalizes_quoted_arch_and_proceeds() {
        // [required] The only coverage of the trim/quote normalization now that
        // parse_docker_arch is inlined: a quoted, whitespace-wrapped arch must
        // still match and let the flow proceed to skopeo.
        let dir = tempdir().unwrap();
        let mock = MockCommandRunner::with_responses(vec![MockResponse::stdout("'arm64'\n")]);
        let handle = mock.clone();
        let ws = crate::Workspace::new(dir.path(), Box::new(mock));

        // Proceeds past the arch gate; later fails on missing unpacked files.
        let _ = extract_rootfs(&ws, "docker-daemon:img", OciArchitecture::Arm64);

        assert!(handle.calls().iter().any(|c| c.cmd == "skopeo"));
    }

    #[test]
    fn extract_rootfs_invokes_skopeo_then_umoci() {
        let dir = tempdir().unwrap();
        let mock = MockCommandRunner::new();
        let handle = mock.clone();
        let ws = crate::Workspace::new(dir.path(), Box::new(mock));
        // Provide a runtime config so copy_runtime_config is skipped.
        fs::write(&ws.runtime_config, b"{}").unwrap();

        // Fails later at move_dir (no unpacked rootfs), but the commands run.
        let _ = extract_rootfs(&ws, "docker://busybox", OciArchitecture::Arm64);

        let calls = handle.calls();
        let skopeo = calls.iter().position(|c| c.cmd == "skopeo").unwrap();
        let umoci = calls.iter().position(|c| c.cmd == "umoci").unwrap();
        assert!(skopeo < umoci, "skopeo must run before umoci");

        let skopeo_args = calls[skopeo].arg_strs();
        assert!(skopeo_args.contains(&"copy".to_string()));
        assert!(skopeo_args.contains(&"--override-arch=arm64".to_string()));
        assert!(skopeo_args.contains(&"docker://busybox".to_string()));

        let umoci_args = calls[umoci].arg_strs();
        assert!(umoci_args.contains(&"unpack".to_string()));
        assert!(umoci_args.contains(&"--rootless".to_string()));
    }
}
