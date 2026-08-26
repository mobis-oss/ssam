// Copyright 2025-2026 Hyundai Mobis Co., Ltd.
// SPDX-License-Identifier: Apache-2.0

use std::collections::HashSet;
use std::io::Write;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Arg, ArgMatches, Command};
use tonic::async_trait;

use crate::client::Client;
use crate::commands::{CommandHandler, write_package_detail};

/// Represents a package URI that can be either a file path or an installed package name.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
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
    const PACKAGE_NAMES: &'static str = "package_names";

    fn write_file_info(
        stdout: &mut dyn Write,
        info: &libssam::InspectLocalPackageResult,
    ) -> Result<()> {
        writeln!(stdout, "Package Information:")?;
        writeln!(stdout, "Info:")?;
        writeln!(stdout, "    Path: {}", info.package_path)?;
        if !info.success {
            if let Some(summary) = &info.error_summary {
                writeln!(stdout, "    Error Summary: {summary}")?;
            }
            if let Some(details) = &info.error_details {
                writeln!(stdout, "    Error Details: {details}")?;
            }
            return Ok(());
        }

        let package_data = info
            .package_data
            .as_ref()
            .context("Local package inspection response missing package data")?;
        writeln!(stdout, "    Name: {}", package_data.package_name)?;
        writeln!(stdout, "    Version: {}", package_data.version)?;
        writeln!(stdout, "    Description: {}", package_data.description)?;
        Ok(())
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
                Arg::new(Self::PACKAGE_NAMES)
                    .help("Package URIs (ssam://<name> or file:///<path>)")
                    .num_args(1..)
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
        let uris = matches
            .get_many::<String>(Self::PACKAGE_NAMES)
            .ok_or_else(|| anyhow::anyhow!("Package URI is required"))?;
        let mut package_names = Vec::new();
        let mut package_paths = Vec::new();
        let mut seen_uris = HashSet::new();

        for uri in uris {
            let package_uri = parse_package_uri(uri)?;
            if !seen_uris.insert(package_uri.clone()) {
                continue;
            }
            match package_uri {
                PackageUri::File(path) => package_paths.push(path),
                PackageUri::Package(name) => package_names.push(name),
            }
        }

        if !package_paths.is_empty() && !client.is_local_mode() {
            anyhow::bail!(
                "file:// URIs are only supported in local mode. \
                 Use 'ssam://<package_name>' to query a remote daemon."
            );
        }

        let mut rendered_any = false;
        let mut package_lookup_failed = false;
        if !package_names.is_empty() {
            let response = client
                .list_package_info(package_names)
                .await
                .context("Failed to get package infos")?;
            for info in response.packages {
                package_lookup_failed |= !info.success;
                if rendered_any {
                    writeln!(stdout)?;
                }
                writeln!(
                    stdout,
                    "Getting package information for: ssam://{}",
                    info.name
                )?;
                writeln!(stdout, "Package Information:")?;
                write_package_detail(stdout, &info).context("Failed to write package detail")?;
                rendered_any = true;
            }
        }

        if !package_paths.is_empty() {
            let paths = package_paths
                .iter()
                .map(|path| path.to_str().context("Non-UTF8 path").map(str::to_owned))
                .collect::<Result<Vec<_>>>()?;
            let response = client
                .list_package_info_from_files(paths)
                .await
                .context("Failed to get package info from file(s)")?;
            if response.packages.len() != package_paths.len() {
                anyhow::bail!(
                    "Invalid list package info from files response: expected {} entries, got {}",
                    package_paths.len(),
                    response.packages.len()
                );
            }
            for (path, info) in package_paths.iter().zip(response.packages.iter()) {
                package_lookup_failed |= !info.success;
                if rendered_any {
                    writeln!(stdout)?;
                }
                writeln!(
                    stdout,
                    "Getting package information for: file://{}",
                    path.to_string_lossy()
                )?;
                Self::write_file_info(stdout, info)?;
                rendered_any = true;
            }
        }

        if package_lookup_failed {
            anyhow::bail!("one or more packages could not be inspected");
        }

        Ok(())
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
        let package_info = client
            .list_package_info_result
            .take()
            .unwrap()
            .unwrap()
            .packages
            .into_iter()
            .next()
            .unwrap();
        client.list_package_info_result = Some(Ok(libssam::ListPackageInfoResponse {
            packages: vec![package_info],
        }));
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
            "    Status: Running\n",
            "    Path: /mock/packages/test-pkg.ssam\n",
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
            "    Status: Broken\n",
            "    Path: /mock/packages/broken-pkg.ssam\n",
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
    async fn test_pkg_info_missing_package_prints_error_and_returns_failure() {
        let mut client = MockClient::new_success();
        client.list_package_info_result = Some(Ok(libssam::ListPackageInfoResponse {
            packages: vec![libssam::PackageInfo {
                success: false,
                name: "missing-pkg".to_owned(),
                status: "NotFound".to_owned(),
                package_path: String::new(),
                package_data: None,
                quota: None,
                error_summary: Some("Package not found: missing-pkg".to_owned()),
                error_details: None,
            }],
        }));
        let mut stdout = Vec::new();

        let handler = PkgInfo;
        let matches = handler
            .command()
            .get_matches_from(vec!["pkg-info", "ssam://missing-pkg"]);

        let result = handler.handle(&matches, &mut client, &mut stdout).await;
        assert!(result.is_err());

        let output = String::from_utf8(stdout).unwrap();
        assert!(output.contains("Getting package information for: ssam://missing-pkg"));
        assert!(output.contains("Name: missing-pkg"));
        assert!(output.contains("Error Summary: Package not found: missing-pkg"));
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
    async fn test_pkg_info_file_uri_routes_to_list_package_info_from_files() {
        let mut client = MockClient::new_success();
        // Clear package lookup so misrouting to it would panic
        client.list_package_info_result = None;
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
    async fn test_pkg_info_pkg_uri_routes_to_list_package_info() {
        let mut client = MockClient::new_success();
        // Clear file lookup so misrouting to it would panic
        client.list_package_info_from_files_result = None;
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

    #[tokio::test(flavor = "current_thread")]
    async fn test_pkg_info_batches_multiple_package_uris() {
        let mut client = MockClient::new_success();
        let mut stdout = Vec::new();
        let handler = PkgInfo;
        let matches = handler.command().get_matches_from(vec![
            "pkg-info",
            "ssam://test-pkg",
            "ssam://broken-pkg",
        ]);

        handler
            .handle(&matches, &mut client, &mut stdout)
            .await
            .unwrap();
        let result = String::from_utf8(stdout).unwrap();

        assert!(result.contains("Getting package information for: ssam://test-pkg"));
        assert!(result.contains("Getting package information for: ssam://broken-pkg"));
        assert!(result.contains("Status: Running"));
        assert!(result.contains("Status: Broken"));
        assert_eq!(
            client
                .captured_list_package_info_names
                .lock()
                .unwrap()
                .as_slice(),
            &[vec!["test-pkg".to_owned(), "broken-pkg".to_owned()]]
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn test_pkg_info_batches_package_and_file_uris() {
        let mut client = MockClient::new_success();
        let mut stdout = Vec::new();
        let handler = PkgInfo;
        let matches = handler.command().get_matches_from(vec![
            "pkg-info",
            "ssam://test-pkg",
            "file:///tmp/test.ssam",
        ]);

        handler
            .handle(&matches, &mut client, &mut stdout)
            .await
            .unwrap();
        let result = String::from_utf8(stdout).unwrap();

        assert!(result.contains("Getting package information for: ssam://test-pkg"));
        assert!(result.contains("Getting package information for: file:///tmp/test.ssam"));
        assert!(result.contains("Status: Running"));
        assert!(result.contains("Path: /mock/packages/test-pkg.ssam"));
        assert_eq!(
            client
                .captured_list_package_info_names
                .lock()
                .unwrap()
                .as_slice(),
            &[vec!["test-pkg".to_owned()]]
        );
        assert_eq!(
            client
                .captured_list_package_info_from_files_paths
                .lock()
                .unwrap()
                .as_slice(),
            &[vec!["/tmp/test.ssam".to_owned()]]
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn test_pkg_info_renders_file_failure_after_success() {
        let mut client = MockClient::new_success();
        let mut package_infos = client
            .list_package_info_from_files_result
            .take()
            .unwrap()
            .unwrap()
            .packages;
        let mut success_info = package_infos.pop().unwrap();
        success_info.package_path = "/tmp/good.ssam".to_owned();
        package_infos.push(success_info);
        package_infos.push(libssam::InspectLocalPackageResult {
            package_data: None,
            package_path: "/nonexistent/path/package.ssam".to_owned(),
            success: false,
            error_summary: Some("Failed to inspect local package".to_owned()),
            error_details: Some("Package not found".to_owned()),
        });
        client.list_package_info_from_files_result =
            Some(Ok(libssam::remocon::ListPackageInfoFromFilesResponse {
                packages: package_infos,
            }));

        let mut stdout = Vec::new();
        let handler = PkgInfo;
        let matches = handler.command().get_matches_from(vec![
            "pkg-info",
            "file:///tmp/good.ssam",
            "file:///nonexistent/path/package.ssam",
        ]);

        let result = handler.handle(&matches, &mut client, &mut stdout).await;

        assert_eq!(
            result.unwrap_err().to_string(),
            "one or more packages could not be inspected"
        );
        let output = String::from_utf8(stdout).unwrap();
        assert!(output.contains("Getting package information for: file:///tmp/good.ssam"));
        assert!(
            output
                .contains("Getting package information for: file:///nonexistent/path/package.ssam")
        );
        assert!(output.contains("Path: /nonexistent/path/package.ssam"));
        assert!(output.contains("Error Summary: Failed to inspect local package"));
        assert!(output.contains("Error Details: Package not found"));
        assert_eq!(
            client
                .captured_list_package_info_from_files_paths
                .lock()
                .unwrap()
                .as_slice(),
            &[vec![
                "/tmp/good.ssam".to_owned(),
                "/nonexistent/path/package.ssam".to_owned()
            ]]
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn test_pkg_info_deduplicates_repeated_uri() {
        let mut client = MockClient::new_success();
        let mut stdout = Vec::new();
        let handler = PkgInfo;
        let matches = handler.command().get_matches_from(vec![
            "pkg-info",
            "ssam://test-pkg",
            "ssam://test-pkg",
        ]);

        handler
            .handle(&matches, &mut client, &mut stdout)
            .await
            .unwrap();
        let result = String::from_utf8(stdout).unwrap();

        assert_eq!(
            result
                .matches("Getting package information for: ssam://test-pkg")
                .count(),
            1
        );
        assert_eq!(
            client
                .captured_list_package_info_names
                .lock()
                .unwrap()
                .as_slice(),
            &[vec!["test-pkg".to_owned()]]
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn test_pkg_info_deduplicates_resolved_package_uri() {
        let mut client = MockClient::new_success();
        let package_info = client
            .list_package_info_result
            .take()
            .unwrap()
            .unwrap()
            .packages
            .into_iter()
            .next()
            .unwrap();
        client.list_package_info_result = Some(Ok(libssam::ListPackageInfoResponse {
            packages: vec![package_info],
        }));
        let mut stdout = Vec::new();
        let handler = PkgInfo;
        let matches =
            handler
                .command()
                .get_matches_from(vec!["pkg-info", "ssam://foo", "ssam:// foo "]);

        handler
            .handle(&matches, &mut client, &mut stdout)
            .await
            .unwrap();

        assert_eq!(
            client
                .captured_list_package_info_names
                .lock()
                .unwrap()
                .as_slice(),
            &[vec!["foo".to_owned()]]
        );
        let output = String::from_utf8(stdout).unwrap();
        assert_eq!(output.matches("Getting package information").count(), 1);
    }
}
