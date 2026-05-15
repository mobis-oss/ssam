// Copyright 2025-2026 Hyundai Mobis Co., Ltd.
// SPDX-License-Identifier: Apache-2.0

use std::io::Write;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Arg, ArgMatches, Command};
use tonic::async_trait;

use crate::client::Client;
use crate::commands::{CommandHandler, write_package_detail};
use libssam::{InspectLocalPackageResponse, JsonResult, PackageInfoResponse};

/// Represents a package URI that can be either a file path or an installed package name.
#[derive(Debug, Clone, PartialEq)]
pub enum PackageUri {
    /// File URI: `<file:///absolute/path>`
    File(PathBuf),
    /// Package URI: `<ssam://package_name>`
    Package(String),
}

/// Parses a package URI string into a `PackageUri` enum.
///
/// Supports two formats:
/// - `file:///absolute/path` → `PackageUri::File(PathBuf)`
/// - `ssam://package_name` → `PackageUri::Package(String)`
///
/// # Errors
///
/// Returns an error if:
/// - The input doesn't match either format
/// - The path or name is empty
pub fn parse_package_uri(input: &str) -> Result<PackageUri> {
    if let Some(path) = input.strip_prefix("file://") {
        if !path.starts_with('/') {
            anyhow::bail!("File path must be absolute. Use 'file:///<absolute_path>'");
        }
        return Ok(PackageUri::File(PathBuf::from(path)));
    }

    if let Some(name) = input.strip_prefix("ssam://") {
        let name = name.trim();
        if name.is_empty() {
            anyhow::bail!("Package name cannot be empty. Use 'ssam://<package_name>'");
        }
        return Ok(PackageUri::Package(name.to_owned()));
    }

    anyhow::bail!(
        "Unsupported input format. Use 'ssam://<package_name>' or 'file:///<absolute_path>'"
    )
}

pub static COMMAND: PkgInfo = PkgInfo;

#[derive(Debug)]
pub struct PkgInfo;

impl PkgInfo {
    const NAME: &'static str = "pkg-info";
    const PACKAGE_NAME: &'static str = "package_name";

    async fn inspect_local_file(
        path: PathBuf,
        client: &mut dyn Client,
        stdout: &mut dyn Write,
    ) -> Result<()> {
        if !client.is_local_mode() {
            anyhow::bail!(
                "file:// URIs are only supported in local mode. \
                 Use 'ssam://<package_name>' to query a remote daemon."
            );
        }
        let path_str = path.to_str().context("Non-UTF8 path")?;
        let json_result = client
            .inspect_local_package(path_str)
            .await
            .context("Failed to inspect local package")?;
        let info = JsonResult::<InspectLocalPackageResponse>::parse_response(&json_result)
            .context("Invalid local package inspection response")?;

        writeln!(stdout, "Package Information:")?;
        writeln!(stdout, "Info:")?;
        writeln!(stdout, "    Path: {}", info.package_path)?;
        writeln!(stdout, "    Name: {}", info.metadata.package_name)?;
        writeln!(stdout, "    Version: {}", info.metadata.version)?;
        writeln!(stdout, "    Description: {}", info.metadata.description)?;
        Ok(())
    }

    async fn show_installed_package(
        name: &str,
        client: &mut dyn Client,
        stdout: &mut dyn Write,
    ) -> Result<()> {
        let json_result = client
            .get_package_info(name)
            .await
            .context("Failed to get package info")?;
        let package_info = JsonResult::<PackageInfoResponse>::parse_response(&json_result)
            .context("Invalid package info response")?;

        writeln!(stdout, "Package Information:")?;
        write_package_detail(stdout, &package_info).context("Failed to write package detail")
    }
}

#[async_trait(?Send)]
impl CommandHandler for PkgInfo {
    fn name(&self) -> &'static str {
        Self::NAME
    }

    fn command(&self) -> Command {
        Command::new(Self::NAME)
            .about("Shows package information")
            .arg(
                Arg::new(Self::PACKAGE_NAME)
                    .help("Package URI (ssam://<name> or file:///<path>)")
                    .required(true)
                    .index(1),
            )
    }

    async fn handle(
        &self,
        matches: &ArgMatches,
        client: &mut dyn Client,
        stdout: &mut dyn Write,
    ) -> Result<()> {
        let uri = matches
            .get_one::<String>(Self::PACKAGE_NAME)
            .ok_or_else(|| anyhow::anyhow!("Package URI is required"))?;

        writeln!(stdout, "Getting package information for: {uri}")?;

        match parse_package_uri(uri)? {
            PackageUri::File(path) => Self::inspect_local_file(path, client, stdout).await,
            PackageUri::Package(name) => Self::show_installed_package(&name, client, stdout).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::MockClient;

    #[test]
    fn test_name() {
        assert_eq!(COMMAND.name(), PkgInfo::NAME);
    }

    // ============================================================================
    // parse_package_uri tests
    // ============================================================================

    // Happy path: file:/// URIs
    #[test]
    fn test_parse_file_uri_simple() {
        let result = parse_package_uri("file:///tmp/test.ssam");
        assert!(result.is_ok());
        let uri = result.unwrap();
        assert_eq!(uri, PackageUri::File(PathBuf::from("/tmp/test.ssam")));
    }

    #[test]
    fn test_parse_file_uri_with_spaces() {
        let result = parse_package_uri("file:///path/with spaces/pkg.ssam");
        assert!(result.is_ok());
        let uri = result.unwrap();
        assert_eq!(
            uri,
            PackageUri::File(PathBuf::from("/path/with spaces/pkg.ssam"))
        );
    }

    #[test]
    fn test_parse_file_uri_deep_path() {
        let result = parse_package_uri("file:///var/lib/ssam/packages/my-pkg.ssam");
        assert!(result.is_ok());
        let uri = result.unwrap();
        assert_eq!(
            uri,
            PackageUri::File(PathBuf::from("/var/lib/ssam/packages/my-pkg.ssam"))
        );
    }

    // Happy path: ssam:// URIs
    #[test]
    fn test_parse_pkg_uri_simple() {
        let result = parse_package_uri("ssam://my-package");
        assert!(result.is_ok());
        let uri = result.unwrap();
        assert_eq!(uri, PackageUri::Package("my-package".to_owned()));
    }

    #[test]
    fn test_parse_pkg_uri_with_version() {
        let result = parse_package_uri("ssam://my-package-1.0");
        assert!(result.is_ok());
        let uri = result.unwrap();
        assert_eq!(uri, PackageUri::Package("my-package-1.0".to_owned()));
    }

    #[test]
    fn test_parse_pkg_uri_with_underscores() {
        let result = parse_package_uri("ssam://my_package_name");
        assert!(result.is_ok());
        let uri = result.unwrap();
        assert_eq!(uri, PackageUri::Package("my_package_name".to_owned()));
    }

    // Error path: bare name (no scheme)
    #[test]
    fn test_parse_bare_name_error() {
        let result = parse_package_uri("my-package");
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("ssam://"),
            "Error message should mention 'ssam://' format"
        );
        assert!(
            err_msg.contains("file:///"),
            "Error message should mention 'file:///' format"
        );
    }

    // Error path: unsupported scheme
    #[test]
    fn test_parse_http_uri_error() {
        let result = parse_package_uri("http://example.com");
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("Unsupported input format"));
    }

    // Error path: file:// with relative path (not absolute)
    #[test]
    fn test_parse_file_relative_path_error() {
        let result = parse_package_uri("file://relative/path");
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("File path must be absolute"));
    }

    // Error path: empty input
    #[test]
    fn test_parse_empty_input_error() {
        let result = parse_package_uri("");
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("Unsupported input format"));
    }

    // file:/// with just root path parses to "/"
    #[test]
    fn test_parse_file_uri_root_path() {
        let result = parse_package_uri("file:///");
        assert!(result.is_ok());
        let uri = result.unwrap();
        assert_eq!(uri, PackageUri::File(PathBuf::from("/")));
    }

    // Error path: ssam:// with empty name
    #[test]
    fn test_parse_pkg_uri_empty_name_error() {
        let result = parse_package_uri("ssam://");
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("Package name cannot be empty"));
    }

    // Error path: ssam:// with whitespace-only name
    #[test]
    fn test_parse_pkg_uri_whitespace_only_error() {
        let result = parse_package_uri("ssam://   ");
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("Package name cannot be empty"));
    }

    // Verify ssam:// trims surrounding whitespace from names
    #[test]
    fn test_parse_pkg_uri_trims_whitespace() {
        let result = parse_package_uri("ssam://  my-package  ");
        assert!(result.is_ok());
        let uri = result.unwrap();
        assert_eq!(uri, PackageUri::Package("my-package".to_owned()));
    }

    // ============================================================================
    // Original pkg-info handler tests
    // ============================================================================

    #[tokio::test(flavor = "current_thread")]
    async fn test_pkg_info_success() {
        let mut client = MockClient::new_success();
        let mut stdout = Vec::new();

        let handler = PkgInfo;
        let matches = handler
            .command()
            .get_matches_from(vec!["pkg-info", "ssam://test-pkg"]);

        handler
            .handle(&matches, &mut client, &mut stdout)
            .await
            .unwrap();
        let result = String::from_utf8(stdout).unwrap();
        let expected = concat!(
            "Getting package information for: ssam://test-pkg\n",
            "Package Information:\n",
            "Info:\n",
            "    Path: /mock/packages/test-pkg.ssam\n",
            "    Status: Running\n",
            "    Version: 1.0.0\n",
            "    Name: test-pkg\n",
            "    Description: Test package\n",
            "\n",
            "Quota:\n",
            "    Enabled: true\n",
            "    Limit: 2048 MB\n",
        );

        assert_eq!(result, expected);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn test_pkg_info_broken_success() {
        let mut client = MockClient::new_success().with_broken_pkg_info();
        let mut stdout = Vec::new();

        let handler = PkgInfo;
        let matches = handler
            .command()
            .get_matches_from(vec!["pkg-info", "ssam://broken-pkg"]);

        handler
            .handle(&matches, &mut client, &mut stdout)
            .await
            .unwrap();
        let result = String::from_utf8(stdout).unwrap();
        let expected = concat!(
            "Getting package information for: ssam://broken-pkg\n",
            "Package Information:\n",
            "Info:\n",
            "    Path: /mock/packages/broken-pkg.ssam\n",
            "    Status: Broken\n",
            "    Name: broken-pkg\n",
            "    Error Summary: Parse error\n",
            "    Error Details: Failed to parse package.toml\n",
        );

        assert_eq!(result, expected);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn test_pkg_info_error() {
        let mut client = MockClient::new_success().with_pkg_info_error();
        let mut stdout = Vec::new();

        let handler = PkgInfo;
        let matches = handler
            .command()
            .get_matches_from(vec!["pkg-info", "ssam://test-pkg"]);

        let result = handler.handle(&matches, &mut client, &mut stdout).await;

        assert!(result.is_err());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn test_pkg_info_empty_name() {
        let mut client = MockClient::new_success();
        let mut stdout = Vec::new();

        let handler = PkgInfo;
        let matches = handler.command().get_matches_from(vec!["pkg-info", "   "]);

        let result = handler.handle(&matches, &mut client, &mut stdout).await;

        assert!(result.is_err());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn test_pkg_info_file_uri_routes_to_inspect_local() {
        let mut client = MockClient::new_success();
        // Clear get_package_info so misrouting to it would panic
        client.get_package_info_result = None;
        let mut stdout = Vec::new();
        let handler = PkgInfo;
        let matches = handler
            .command()
            .get_matches_from(vec!["pkg-info", "file:///tmp/test.ssam"]);
        handler
            .handle(&matches, &mut client, &mut stdout)
            .await
            .unwrap();
        let result = String::from_utf8(stdout).unwrap();
        assert!(
            result.contains("Package Information"),
            "should show package info"
        );
        assert!(
            result.contains("/mock/packages/test-pkg.ssam"),
            "should show path from mock response"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn test_pkg_info_pkg_uri_routes_to_get_package_info() {
        let mut client = MockClient::new_success();
        // Clear inspect_local_package so misrouting to it would panic
        client.inspect_local_package_result = None;
        let mut stdout = Vec::new();
        let handler = PkgInfo;
        let matches = handler
            .command()
            .get_matches_from(vec!["pkg-info", "ssam://test-pkg"]);
        handler
            .handle(&matches, &mut client, &mut stdout)
            .await
            .unwrap();
        let result = String::from_utf8(stdout).unwrap();
        assert!(
            result.contains("Status: Running"),
            "ssam:// should show status"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn test_pkg_info_file_uri_rejected_in_remote_mode() {
        let mut client = MockClient::new_success().with_remote_mode();
        let mut stdout = Vec::new();
        let handler = PkgInfo;
        let matches = handler
            .command()
            .get_matches_from(vec!["pkg-info", "file:///tmp/test.ssam"]);
        let result = handler.handle(&matches, &mut client, &mut stdout).await;
        assert!(result.is_err(), "file:// should be rejected in remote mode");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("local mode"),
            "error should mention local mode"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn test_pkg_info_bare_name_returns_error() {
        let mut client = MockClient::new_success();
        let mut stdout = Vec::new();
        let handler = PkgInfo;
        let matches = handler
            .command()
            .get_matches_from(vec!["pkg-info", "my-package"]);
        let result = handler.handle(&matches, &mut client, &mut stdout).await;
        assert!(result.is_err(), "bare name should return error");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("ssam://") || err.contains("file:///"),
            "error should mention URI schemes"
        );
    }
}
