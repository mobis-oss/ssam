// Copyright 2025-2026 Hyundai Mobis Co., Ltd.
// SPDX-License-Identifier: Apache-2.0

use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result};
use clap::{Arg, ArgMatches, Command};
use tonic::async_trait;

use crate::client::Client;
use crate::commands::CommandHandler;
use libssam::{InstallResponse, JsonResult};

pub static COMMAND: Install = Install;

#[derive(Debug)]
pub struct Install;

impl Install {
    const NAME: &'static str = "install";
    const PACKAGE_PATH: &'static str = "package_path";
    const INSTALL_FORCE: &'static str = "force";
    const REMOVE_DATA: &'static str = "remove_data";

    fn canonicalize_relative(package_path: &str) -> Result<String> {
        let canonical_path = std::fs::canonicalize(package_path)
            .with_context(|| format!("Failed to canonicalize path: {package_path}"))?;

        canonical_path
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("Path contains invalid UTF-8: {package_path}"))
            .map(ToOwned::to_owned)
    }
}

#[async_trait(?Send)]
impl CommandHandler for Install {
    fn name(&self) -> &'static str {
        Self::NAME
    }

    fn command(&self) -> Command {
        Command::new(Self::NAME)
            .about("Installs a package")
            .arg(
                Arg::new(Self::PACKAGE_PATH)
                    .help("Path to the package file")
                    .required(true)
                    .index(1),
            )
            .arg(
                Arg::new(Self::INSTALL_FORCE)
                    .short('f')
                    .long(Self::INSTALL_FORCE)
                    .help(
                        "Force install the package, even if it is lower version than the installed one",
                    )
                    .action(clap::ArgAction::SetTrue),
            )
            .arg(
                Arg::new(Self::REMOVE_DATA)
                    .short('r')
                    .long(Self::REMOVE_DATA)
                    .help("Remove all data associated with the package when removing")
                    .action(clap::ArgAction::SetTrue),
            )
    }

    async fn handle(
        &self,
        matches: &ArgMatches,
        client: &mut dyn Client,
        stdout: &mut dyn Write,
    ) -> Result<()> {
        let package_path = matches
            .get_one::<String>(Self::PACKAGE_PATH)
            .ok_or_else(|| anyhow::anyhow!("Package path is required"))?;
        let force = matches.get_flag(Self::INSTALL_FORCE);
        let remove_data = matches.get_flag(Self::REMOVE_DATA);

        let is_local_mode = client.is_local_mode();
        let path = Path::new(package_path);

        // Resolve path based on mode and path type
        let processed_path = if path.is_absolute() {
            package_path.to_owned()
        } else if is_local_mode {
            Self::canonicalize_relative(package_path)?
        } else {
            anyhow::bail!(
                "Relative paths are not supported in remote mode. \
                 Please provide an absolute path."
            );
        };

        if !is_local_mode {
            eprintln!("Warning: Absolute path may not work as intended in remote mode");
        }

        writeln!(stdout, "Installing package: {processed_path}")?;
        let json_result = client
            .install_package(&processed_path, force, remove_data)
            .await?;
        let InstallResponse { metadata } =
            JsonResult::<InstallResponse>::parse_response(&json_result)?;
        writeln!(
            stdout,
            "Installed: {} ({})",
            metadata.package_name, metadata.version
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::MockClient;
    use crate::commands::CommandHandler;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_name() {
        assert_eq!(COMMAND.name(), Install::NAME);
    }

    fn create_local_test_file() -> anyhow::Result<String> {
        let cwd = std::env::current_dir().context("Failed to get current directory")?;
        let unique = format!(
            "ssam-install-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        );

        let dir = cwd.join("target").join(unique);
        fs::create_dir_all(&dir).context("Failed to create test directory")?;

        let file = dir.join("dummy.ssam");
        fs::write(&file, b"dummy").context("Failed to write test file")?;

        let relative = file
            .strip_prefix(&cwd)
            .context("Failed to strip cwd prefix")?
            .to_string_lossy()
            .to_string();

        Ok(relative)
    }

    #[tokio::test(flavor = "current_thread")]
    async fn test_install_success() {
        let mut client = MockClient::new_success();
        let handler = Install;
        let cmd = handler.command();

        let matches = cmd
            .try_get_matches_from(["install", "/path/to/pkg.ssam"])
            .unwrap();

        let mut stdout = Vec::new();
        handler
            .handle(&matches, &mut client, &mut stdout)
            .await
            .unwrap();
        let result = String::from_utf8(stdout).unwrap();

        assert!(result.contains("Installing package"));
        assert!(result.contains("Installed: test-pkg (1.0.0)"));
        assert!(result.contains("/path/to/pkg.ssam"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn test_install_error() {
        let mut client = MockClient::new_success().with_install_error();
        let handler = Install;
        let cmd = handler.command();

        let matches = cmd
            .try_get_matches_from(["install", "/path/to/pkg.ssam"])
            .unwrap();

        let mut stdout = Vec::new();
        let result = handler.handle(&matches, &mut client, &mut stdout).await;

        assert!(result.is_err());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn test_install_missing_metadata_error() {
        let mut client = MockClient::new_success();
        client.install_package_result = Some(Ok(
            r#"{"schema_version":"1","success":true,"data":{}}"#.to_string(),
        ));

        let handler = Install;
        let cmd = handler.command();
        let matches = cmd
            .try_get_matches_from(["install", "/path/to/pkg.ssam"])
            .unwrap();

        let mut stdout = Vec::new();
        let result = handler.handle(&matches, &mut client, &mut stdout).await;

        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Failed to parse JSON response")
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn test_install_remote_relative_path_error() {
        let mut client = MockClient::new_success().with_remote_mode();
        let handler = Install;
        let cmd = handler.command();

        let matches = cmd
            .try_get_matches_from(["install", "relative/path/pkg.ssam"])
            .unwrap();

        let mut stdout = Vec::new();
        let result = handler.handle(&matches, &mut client, &mut stdout).await;

        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Relative paths are not supported in remote mode")
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn test_install_remote_absolute_path_success() {
        let mut client = MockClient::new_success().with_remote_mode();
        let handler = Install;
        let cmd = handler.command();

        let matches = cmd
            .try_get_matches_from(["install", "/absolute/path/pkg.ssam"])
            .unwrap();

        let mut stdout = Vec::new();
        let result = handler.handle(&matches, &mut client, &mut stdout).await;

        assert!(result.is_ok());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn test_install_local_relative_path_success() {
        let backup_cwd = std::env::current_dir().expect("Failed to get current directory");
        let tempdir = TempDir::new().expect("Failed to create temporary directory");
        std::env::set_current_dir(tempdir.path()).expect("Failed to set current directory");
        let relative_path = create_local_test_file().unwrap();
        let expected = std::fs::canonicalize(&relative_path)
            .unwrap()
            .to_string_lossy()
            .to_string();

        let mut client = MockClient::new_success();
        let handler = Install;
        let cmd = handler.command();

        let matches = cmd
            .try_get_matches_from(["install", &relative_path])
            .unwrap();

        let mut stdout = Vec::new();
        handler
            .handle(&matches, &mut client, &mut stdout)
            .await
            .unwrap();

        std::env::set_current_dir(backup_cwd).expect("Failed to restore current directory");

        let result = String::from_utf8(stdout).unwrap();
        assert!(result.contains(&expected));
    }

    #[test]
    fn test_canonicalize_relative_nonexistent_path_error() {
        let err = Install::canonicalize_relative("this/path/should/not/exist.ssam").unwrap_err();

        assert!(
            err.to_string().contains("Failed to canonicalize path"),
            "unexpected error: {err:?}"
        );
    }
}
