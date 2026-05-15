// Copyright 2025-2026 Hyundai Mobis Co., Ltd.
// SPDX-License-Identifier: Apache-2.0

use anyhow::{Context, Result};
use std::path::Path;
use std::path::PathBuf;

use super::ImageType;
use super::execute_command;

mod rootfs {
    use anyhow::{Context, Result};
    use oci_spec::runtime::{Mount, Spec};

    use std::fs;
    use std::path::{Path, PathBuf};

    struct Rootfs {
        rootfs: PathBuf,
    }

    impl Rootfs {
        fn new(rootfs: impl AsRef<Path>) -> Self {
            Self {
                rootfs: rootfs.as_ref().to_path_buf(),
            }
        }

        fn as_path(&self) -> &Path {
            &self.rootfs
        }
    }

    fn load_spec(config_path: impl AsRef<Path>) -> Result<Spec> {
        Spec::load(&config_path).context(format!(
            "Failed to load container runtime spec from {}",
            config_path.as_ref().display()
        ))
    }

    fn get_mounts(spec: &Spec) -> Option<&[Mount]> {
        spec.mounts().as_ref().map(Vec::as_slice)
    }

    fn is_systemd_notify_type() -> bool {
        println!("TODO!: Implement actual systemd notify type check!!!");
        false
    }

    const BASE_MANDATORY_MOUNTS: [&str; 2] = ["/dev", "/dev/pts"];
    const BASE_OPTIONAL_MOUNTS: [&str; 3] = ["/proc", "/sys", "/tmp"];
    const RUN: &str = "/run";

    fn mounts_list<'m>(base: &'m [&'m str], add_run: bool) -> Vec<&'m Path> {
        base.iter()
            .copied()
            .chain(add_run.then_some(RUN))
            .map(Path::new)
            .collect()
    }

    fn mount_destinations(mounts: &[Mount]) -> Vec<&Path> {
        mounts.iter().map(|m| m.destination().as_path()).collect()
    }

    fn find_missing_path<'p, P>(mandatory: &'p [P], mount_dests: &[P]) -> Option<&'p P>
    where
        P: AsRef<Path>,
    {
        mandatory.iter().find(|m| {
            let m_path = m.as_ref();
            !mount_dests.iter().any(|d| d.as_ref() == m_path)
        })
    }

    // Safety: Using Debug format ({:?}) for PathBuf instead of Display to prevent
    // log injection attacks via special characters in path names.
    #[allow(clippy::use_debug, clippy::unnecessary_debug_formatting)]
    fn create_dirs(paths: &[PathBuf]) -> Result<()> {
        for p in paths {
            println!("Warning: Creating missing directory: {}", p.display());
            fs::create_dir_all(p).context(format!("Failed to create {p:?}"))?;
        }
        Ok(())
    }

    fn ensure_mandatory_dirs(rootfs: impl AsRef<Path>, mandatory: &[&Path]) -> Result<()> {
        let rootfs = rootfs.as_ref();
        let missing_dirs: Vec<PathBuf> = mandatory
            .iter()
            .copied()
            .filter(|&m| m != Path::new("/dev/pts"))
            .map(|m| rootfs.join(m.strip_prefix("/").unwrap_or(m)))
            .filter(|p| !p.exists())
            .collect();
        create_dirs(&missing_dirs)
    }

    fn ensure_optional_dirs(
        rootfs: impl AsRef<Path>,
        optional: &[&Path],
        mount_dests: &[&Path],
    ) -> Result<()> {
        let rootfs = rootfs.as_ref();

        // Create directories for optional mounts that are in the config but not in the filesystem
        optional
            .iter()
            .copied()
            .filter(|&path| mount_dests.contains(&path))
            .map(|path| rootfs.join(path.strip_prefix("/").unwrap_or(path)))
            .filter(|path| !path.exists())
            .try_for_each(|path| {
                println!(
                    "Warning: {} does not exist in {}, creating it",
                    path.display(),
                    rootfs.display()
                );
                fs::create_dir_all(&path).context(format!(
                    "Failed to create {} in {}",
                    path.display(),
                    rootfs.display()
                ))
            })?;

        // Warn about optional mounts that are in the config but not in the filesystem
        optional
            .iter()
            .copied()
            .filter(|&path| !mount_dests.contains(&path))
            .map(|path| rootfs.join(path.strip_prefix("/").unwrap_or(path)))
            .filter(|path| path.exists())
            .for_each(|path| {
                println!("Warning: {} is missing in config.json", path.display());
            });
        Ok(())
    }

    fn ensure_mounts_config(
        rootfs: &Rootfs,
        oci_runtime_conf_file: impl AsRef<Path>,
    ) -> Result<()> {
        let oci_runtime_conf = load_spec(&oci_runtime_conf_file)?;
        let mounts = get_mounts(&oci_runtime_conf).ok_or(anyhow::anyhow!(
            "No mounts found at {}",
            oci_runtime_conf_file.as_ref().display()
        ))?;
        let systemd_notify = is_systemd_notify_type();
        let mandatory = mounts_list(&BASE_MANDATORY_MOUNTS, systemd_notify);
        let optional = mounts_list(&BASE_OPTIONAL_MOUNTS, !systemd_notify);
        let mount_dests = mount_destinations(mounts);

        if let Some(missing) = find_missing_path(&mandatory, &mount_dests) {
            anyhow::bail!(
                "Required mount '{}' is missing from config.json mounts. Please add it.",
                missing.display()
            );
        }

        ensure_mandatory_dirs(rootfs.as_path(), &mandatory)?;
        ensure_optional_dirs(rootfs.as_path(), &optional, &mount_dests)?;
        Ok(())
    }

    pub fn prepare(
        rootfs: impl AsRef<Path>,
        oci_runtime_conf_file: impl AsRef<Path>,
    ) -> Result<()> {
        let rootfs = Rootfs::new(rootfs);

        ensure_mounts_config(&rootfs, oci_runtime_conf_file)?;
        Ok(())
    }
}

fn build_pkgfs_ext4_image(src: &str, dest: &str, mkfs_args: &[&str]) -> Result<()> {
    let mut mkfs_args = Vec::from(mkfs_args);

    let dir_size = fs_extra::dir::get_size(src)
        .context(anyhow::anyhow!("Failed to get size of pkgfs: {src}"))?;
    println!("Size of package source directory: {dir_size} bytes");
    // Calculate the size in MiB, adding 10% overhead
    let overhead = dir_size.div_ceil(10);
    let total_size = dir_size + overhead;
    // Lower than 2 MiB makes ext2 forcibly
    let total_size_mb = total_size.div_ceil(1024 * 1024).max(3);
    println!("10% overhead added source directory: {total_size} bytes ({total_size_mb} MiB)");

    execute_command(
        "truncate",
        Some(&["-s", &format!("{total_size_mb}M"), dest]),
        false,
    )?;

    mkfs_args.push("-d");
    mkfs_args.push(src);
    mkfs_args.push(dest);
    execute_command("fakeroot", Some(mkfs_args), false)?;
    Ok(())
}

fn build_pkgfs_erofs_image(src: &str, dest: &str, mkfs_args: &[&str]) -> Result<()> {
    let mut mkfs_args = Vec::from(mkfs_args);
    mkfs_args.push(dest);
    mkfs_args.push(src);
    execute_command("fakeroot", Some(mkfs_args), false)?;
    Ok(())
}

fn build_pkgfs_image(
    workspace: &crate::Workspace,
    pkgfs_src_path: impl AsRef<Path>,
    image_type: ImageType,
) -> Result<PathBuf> {
    let (image_file_name, mkfs_args) = image_type.into();
    let pkgfs_src_path = pkgfs_src_path.as_ref();

    println!("Building Package filesystem image: {image_file_name} with {mkfs_args:?}",);

    workspace.prepare_intermediate_dir()?;
    let image_file_path = workspace.intermediate_dir.join(image_file_name);
    if image_file_path.exists() {
        println!(
            "WARN: Package filesystem image file {} already exists, will be overwritten!",
            image_file_path.display()
        );
    }

    let image_file_path_str = image_file_path.to_str().ok_or(anyhow::anyhow!(format!(
        r#"Image filename "{}" contains invalid character!"#,
        image_file_path.display()
    )))?;

    let pkgfs_src_path_str = pkgfs_src_path.to_str().ok_or(anyhow::anyhow!(format!(
        r#"Package source path "{}" contains invalid character!"#,
        pkgfs_src_path.display()
    )))?;

    match image_type {
        ImageType::Erofs | ImageType::ErofsLz4 | ImageType::ErofsLz4hc => {
            build_pkgfs_erofs_image(pkgfs_src_path_str, image_file_path_str, &mkfs_args)?;
        }
        ImageType::Ext4 => {
            build_pkgfs_ext4_image(pkgfs_src_path_str, image_file_path_str, &mkfs_args)?;
        }
    }

    Ok(image_file_path)
}

pub fn create(
    workspace: &crate::Workspace,
    pkgfs_src: impl AsRef<Path>,
    image_type: ImageType,
) -> Result<PathBuf> {
    let pkgfs_src = pkgfs_src.as_ref();

    println!(
        "# Creating {} image from directory: {}",
        image_type,
        pkgfs_src.display()
    );

    rootfs::prepare(pkgfs_src, &workspace.runtime_config)?;
    build_pkgfs_image(workspace, pkgfs_src, image_type)
}
