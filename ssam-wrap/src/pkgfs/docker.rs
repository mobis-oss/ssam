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
            let arch = arch.trim();
            let arch = if arch.starts_with('\'') && arch.ends_with('\'') && arch.len() > 1 {
                &arch[1..arch.len() - 1]
            } else {
                arch
            };
            if arch != architecture.to_string() {
                return Err(anyhow::anyhow!(
                    "Docker image {} is not for {} architecture, found: {}",
                    docker_uri,
                    architecture,
                    arch.trim()
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
