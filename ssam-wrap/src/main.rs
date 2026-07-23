// Copyright 2025-2026 Hyundai Mobis Co., Ltd.
// SPDX-License-Identifier: Apache-2.0

use anyhow::Context;
use clap::{Args, CommandFactory, FromArgMatches, Parser};
use command::CommandRunner;
use libssam::ssam_package::{PackageFile, PackageFilesystem};
use libssam::superblock;
use serde_json::Value;
use std::path::Path;
use std::{fs, path::PathBuf};

mod command;
mod pkgfs;
mod veritysetup;

include!(concat!(env!("OUT_DIR"), "/runtime_config_template.rs"));

const PACKAGE_CONFIG_FILENAME: &str = "config.toml";
const RUNTIME_CONFIG_FILENAME: &str = "runtime.json";
const SECCOMP_POLICY_FILENAME: &str = "seccomp.json";
const PACKAGE_FILESYSTEM_PATH: &str = "pkgfs";
const PACKAGE_INTERMEDIATE_PATH: &str = "intermediate";

#[derive(Args, Debug)]
#[group(id = "wrap_only_args", multiple = true, required = false)]
struct WrapOnlyArgs {
    /// Specify private key file to sign the package
    #[arg(short = 'k', long = "private-key")]
    private_key_file: Option<String>,

    /// Specify output package filename [default: <WORKSPACE>/<name>-<version>.ssam]
    #[arg(short = 'o', long = "output")]
    package_output_filename: Option<String>,

    /// Specify package filesystem type.
    /// Only required when package source is a directory.
    /// If not specified, the default type is `erofs-lz4hc`.
    #[arg(short = 't', long)]
    pkgfs_type: Option<pkgfs::ImageType>,
}

#[derive(Parser, Debug)]
struct Cli {
    #[command(flatten)]
    wrap_only_args: WrapOnlyArgs,

    /// Run as prepare mode
    #[arg(
        short = 'p',
        long,
        value_name = "PACKAGE_NAME",
        conflicts_with = "wrap_only_args"
    )]
    prepare: Option<String>,

    // Help message is set in main() at runtime
    #[arg(short = 's', long)]
    pkgfs_src: Option<String>,

    /// Specify source image architecture when using container transport for --pkgfs-src.
    /// Only effective with container transports (docker://, docker-daemon:, etc.);
    /// ignored otherwise. Defaults to arm64 if not specified.
    #[arg(short = 'a', long = "src-oci-arch")]
    src_oci_arch: Option<pkgfs::OciArchitecture>,

    #[arg(required = true)]
    workspace: String,
}

#[derive(Debug)]
struct Workspace {
    path: PathBuf,
    runtime_config: PathBuf,
    package_config: PathBuf,
    seccomp_policy: PathBuf,
    pkgfs: PathBuf,
    intermediate_dir: PathBuf,
    /// Runner used for all external command execution. Owned so `Workspace` is
    /// self-contained; tests pass a `Box<MockCommandRunner>` and keep a shared
    /// clone to inspect recorded calls.
    runner: Box<dyn CommandRunner>,
}

impl Workspace {
    fn new(workspace_root: impl AsRef<Path>, runner: Box<dyn CommandRunner>) -> Self {
        let workspace_root = workspace_root.as_ref();
        let workspace_root = if workspace_root.is_absolute() {
            workspace_root.to_path_buf()
        } else {
            // Using absolute() as workspace_root could not exist yet
            std::path::absolute(
                // Check if running in AppImage environment
                // Refer to Environment variables of AppImage Packaging Guide:
                // > https://docs.appimage.org/packaging-guide/environment-variables.html
                // * APPIMAGE: (Absolute) path to AppImage file (with symlinks resolved)
                // * OWD: Path to working directory at the time the AppImage is called
                match (std::env::var_os("APPIMAGE"), std::env::var_os("OWD")) {
                    (Some(_), Some(owd)) if !owd.is_empty() => {
                        // Running in AppImage: resolve relative to OWD
                        let owd_path = PathBuf::from(owd);
                        owd_path.join(workspace_root)
                    }
                    _ => workspace_root.to_path_buf(),
                },
            )
            .unwrap_or_else(|e| {
                panic!(
                    "Given workspace path '{}' is wrong: {:?}",
                    workspace_root.display(),
                    e
                )
            })
        };

        assert!(
            !workspace_root.exists() || workspace_root.is_dir(),
            "Workspace path is not a directory: {}",
            workspace_root.display()
        );

        let runtime_config = workspace_root.join(RUNTIME_CONFIG_FILENAME);
        let package_config = workspace_root.join(PACKAGE_CONFIG_FILENAME);
        let seccomp_policy = workspace_root.join(SECCOMP_POLICY_FILENAME);
        let pkgfs = workspace_root.join(PACKAGE_FILESYSTEM_PATH);
        let intermediate_dir = workspace_root.join(PACKAGE_INTERMEDIATE_PATH);

        // To return Self without Result, using panic!()
        fs::create_dir_all(&workspace_root).unwrap_or_else(|e| {
            panic!(
                "Failed to create workspace directory: {} with error {:?}",
                workspace_root.display(),
                e
            )
        });

        Self {
            path: workspace_root,
            runtime_config,
            package_config,
            seccomp_policy,
            pkgfs,
            intermediate_dir,
            runner,
        }
    }

    fn prepare(&self, name: &str) -> anyhow::Result<()> {
        if self.package_config.exists() {
            println!("Package configuration file already exists. Checking...");
            println!(
                "TODO: Implement checking whether valid such as package name is the same as given."
            );
        } else {
            println!(
                "Generating package configuration file: {}",
                self.package_config.display()
            );
            let config = libssam::config::print_package_config_spec().replace(
                "[package]\nname = \"package_name\"",
                &format!("[package]\nname = \"{name}\""),
            );
            fs::write(&self.package_config, config)
                .context("Failed to generate package configuration file")?;
        }

        if self.runtime_config.exists() {
            println!(
                r#"Keeping existing runtime configuration file "{}"."#,
                self.runtime_config.display()
            );
        } else {
            println!(
                "Generating runtime configuration file: {}",
                self.runtime_config.display()
            );

            let json_value: Value = serde_json::from_str(RUNTIME_CONFIG_TEMPLATE)?;
            fs::write(
                &self.runtime_config,
                serde_json::to_string_pretty(&json_value)?,
            )
            .context("Failed to generate pretty runtime config file")?;
        }

        if self.seccomp_policy.exists() {
            println!(
                r#"Keeping existing seccomp policy file "{}"."#,
                self.seccomp_policy.display()
            );
        } else {
            println!(
                "Generating default seccomp policy file: {}",
                self.seccomp_policy.display()
            );

            let json_value: Value = serde_json::from_str(libssam::seccomp::DEFAULT_SECCOMP_POLICY)?;
            fs::write(
                &self.seccomp_policy,
                serde_json::to_string_pretty(&json_value)?,
            )
            .context("Failed to generate default seccomp policy file")?;
        }

        Ok(())
    }

    fn prepare_intermediate_dir(&self) -> anyhow::Result<()> {
        if !self.intermediate_dir.exists() {
            fs::create_dir_all(&self.intermediate_dir).context(format!(
                "Failed to create intermediate directory: {}",
                self.intermediate_dir.display()
            ))?;
        } else if !self.intermediate_dir.is_dir() {
            anyhow::bail!(
                "Location of the intermediate directory already exists and is not a directory: {}",
                self.intermediate_dir.display()
            );
        }
        Ok(())
    }
}

fn root_hash_path(image_file: &Path, intermediate_dir: &Path) -> anyhow::Result<PathBuf> {
    let filename = image_file.file_name().with_context(|| {
        format!(
            "Unable to get filename from pkgfs image file: {}",
            image_file.display()
        )
    })?;
    let mut name = filename.to_os_string();
    name.push(".root_hash");
    Ok(intermediate_dir.join(name))
}

fn resolve_output_path(
    explicit: Option<&str>,
    workspace_path: &Path,
    name: &str,
    version: &str,
) -> PathBuf {
    match explicit {
        Some(filename) => PathBuf::from(filename),
        None => workspace_path.join(format!("{name}-{version}.ssam")),
    }
}

fn parse_cli() -> anyhow::Result<Cli> {
    // Injecting the long help for `--pkgfs-src` at runtime.
    let supported_transports_help = format!(
        "Specify package filesystem source.\nSupported container transports:\n{}",
        pkgfs::SUPPORTED_CONTAINER_TRANSPORTS
            .iter()
            .map(|t| format!("  {t}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
    Ok(Cli::from_arg_matches(
        &Cli::command()
            .mut_arg("pkgfs_src", |a| a.long_help(supported_transports_help))
            .get_matches(),
    )?)
}

fn run(args: Cli, runner: Box<dyn CommandRunner>) -> anyhow::Result<()> {
    let workspace = Workspace::new(&args.workspace, runner);

    // TODO
    // If given pkgfs_src is a directory outside of the workspace,
    // workspace.pkgfs_src and the result of pkgfs::prepare() would
    // point different location.
    // This could cause confusion, so might need to fix.
    pkgfs::prepare(&workspace, args.pkgfs_src.as_deref(), args.src_oci_arch)?;
    if let Some(name) = &args.prepare {
        workspace.prepare(name)?;
        return Ok(());
    }

    workspace.prepare_intermediate_dir()?;

    let private_key_filename = args
        .wrap_only_args
        .private_key_file
        .as_deref()
        .context("Private key file path is required")?;

    let pkgfs_image_file = pkgfs::build_image(&workspace, args.wrap_only_args.pkgfs_type)?;

    let root_hash_filepath = root_hash_path(&pkgfs_image_file, &workspace.intermediate_dir)?;

    let verity_info =
        veritysetup::FormatVerity::new(&pkgfs_image_file, Some(&root_hash_filepath), true, vec![])?
            .run(workspace.runner.as_ref())?;

    let pkgfs_type = superblock::FileSystemSuperBlockBroker::new(&pkgfs_image_file)?.fs_type();

    let pkgfs_info = PackageFilesystem::from_source(&pkgfs_image_file, pkgfs_type, verity_info);

    let pkg_file = PackageFile::from_source(
        workspace.package_config,
        workspace.runtime_config,
        workspace.seccomp_policy,
        &pkgfs_info,
    )?;
    let metadata = pkg_file.metadata();
    let package_filepath = resolve_output_path(
        args.wrap_only_args.package_output_filename.as_deref(),
        &workspace.path,
        &metadata.package.name,
        &metadata.package.version,
    );
    pkg_file.wrap(&package_filepath, private_key_filename)?;

    println!(
        r#"Package "{}" has been created with below information"#,
        package_filepath.display()
    );
    println!("* Package name: {}", metadata.package.name);
    println!(
        "* Container service type: {:?}",
        metadata.service.service_type
    );
    println!("* Signed with key: {private_key_filename}");
    println!(
        "* Signed dm-verity table: {}",
        pkgfs_info.verity_info().table_params
    );
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let args = parse_cli()?;
    run(args, Box::new(command::SystemCommandRunner))
}
