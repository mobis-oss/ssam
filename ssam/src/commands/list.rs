// Copyright 2025-2026 Hyundai Mobis Co., Ltd.
// SPDX-License-Identifier: Apache-2.0

use std::io::{self, Write};

use anyhow::{Context, Result};
use clap::{Arg, ArgAction, ArgMatches, Command};
use tonic::async_trait;

use crate::client::Client;
use crate::commands::{CommandHandler, write_package_detail};
use libssam::{JsonResult, PackageInfoResponse, PackageStatusEntry};

pub static COMMAND: List = List;

#[derive(Debug)]
pub struct List;

impl List {
    const NAME: &'static str = "list";
    const DETAIL: &'static str = "detail";
}

fn get_terminal_width() -> usize {
    term_size::dimensions().map_or(80, |(w, _)| w)
}

fn write_detail_list(stdout: &mut dyn Write, packages: &[PackageInfoResponse]) -> io::Result<()> {
    if packages.is_empty() {
        return writeln!(stdout, "No packages found");
    }

    writeln!(stdout, "List of packages (detailed)")?;
    for (idx, info) in packages.iter().enumerate() {
        if idx > 0 {
            writeln!(stdout)?;
        }
        writeln!(stdout, "Package: {}", info.name)?;
        write_package_detail(stdout, info)?;
    }
    Ok(())
}

fn write_table_list(stdout: &mut dyn Write, packages: &[PackageStatusEntry]) -> io::Result<()> {
    const HEADER_NAME: &str = "Package name";
    const HEADER_STATUS: &str = "Status";
    const COLUMN_GAP: usize = 4;

    if packages.is_empty() {
        return writeln!(stdout, "No packages found");
    }

    let max_name_width = packages
        .iter()
        .map(|p| p.package_name.len())
        .max()
        .unwrap_or(0)
        .max(HEADER_NAME.len());

    let name_col_width = max_name_width + COLUMN_GAP;
    let bar_width = (name_col_width + HEADER_STATUS.len() + 20).min(get_terminal_width());

    writeln!(stdout)?;
    writeln!(stdout, "List of packages")?;
    writeln!(stdout, "{HEADER_NAME:<name_col_width$}{HEADER_STATUS}")?;
    writeln!(stdout, "{:=<bar_width$}", "")?;
    for p in packages {
        writeln!(stdout, "{:<name_col_width$}{}", p.package_name, p.status)?;
    }
    Ok(())
}

#[async_trait(?Send)]
impl CommandHandler for List {
    fn name(&self) -> &'static str {
        Self::NAME
    }

    fn command(&self) -> Command {
        Command::new(Self::NAME).about("Lists all packages").arg(
            Arg::new(Self::DETAIL)
                .long(Self::DETAIL)
                .help("Show detailed information for each package")
                .action(ArgAction::SetTrue),
        )
    }

    async fn handle(
        &self,
        matches: &ArgMatches,
        client: &mut dyn Client,
        stdout: &mut dyn Write,
    ) -> Result<()> {
        if matches.get_flag(Self::DETAIL) {
            let json_result = client.get_all_package_info().await?;
            let packages = JsonResult::<Vec<PackageInfoResponse>>::parse_response(&json_result)?;
            write_detail_list(stdout, &packages).context("Failed to write detail list")
        } else {
            let json_result = client.list_packages_status().await?;
            let packages = JsonResult::<Vec<PackageStatusEntry>>::parse_response(&json_result)?;
            write_table_list(stdout, &packages).context("Failed to write package list")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::MockClient;

    #[test]
    fn test_name() {
        assert_eq!(COMMAND.name(), List::NAME);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn test_list_empty() {
        let empty_data: Vec<libssam::PackageStatusEntry> = vec![];
        let json_result = libssam::JsonResult::success(empty_data);

        let mut client = MockClient {
            list_packages_result: Some(Ok(json_result)),
            ..MockClient::new_success()
        };

        let handler = List;
        let matches = handler.command().get_matches_from(vec!["list"]);
        let mut stdout = Vec::new();

        handler
            .handle(&matches, &mut client, &mut stdout)
            .await
            .unwrap();
        let result = String::from_utf8(stdout).unwrap();

        assert!(result.contains("No packages found"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn test_list_with_packages() {
        let mut client = MockClient::new_success();
        let handler = List;
        let matches = handler.command().get_matches_from(vec!["list"]);
        let mut stdout = Vec::new();

        handler
            .handle(&matches, &mut client, &mut stdout)
            .await
            .unwrap();
        let result = String::from_utf8(stdout).unwrap();

        assert!(result.contains("List of packages"));
        assert!(result.contains("Package name"));
        assert!(result.contains("Status"));
        assert!(result.contains("test-pkg"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn test_list_multiple_packages() {
        let test_data = vec![
            libssam::PackageStatusEntry {
                package_name: "test-pkg".to_owned(),
                status: "running".to_owned(),
            },
            libssam::PackageStatusEntry {
                package_name: "extra-pkg".to_owned(),
                status: "stopped".to_owned(),
            },
        ];
        let json_result = libssam::JsonResult::success(test_data);

        let mut client = MockClient {
            list_packages_result: Some(Ok(json_result)),
            ..MockClient::new_success()
        };
        let handler = List;
        let matches = handler.command().get_matches_from(vec!["list"]);
        let mut stdout = Vec::new();

        handler
            .handle(&matches, &mut client, &mut stdout)
            .await
            .unwrap();
        let result = String::from_utf8(stdout).unwrap();

        assert!(result.contains("test-pkg"));
        assert!(result.contains("extra-pkg"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn test_list_error() {
        let mut client = MockClient::new_success().with_list_error();
        let handler = List;
        let matches = handler.command().get_matches_from(vec!["list"]);
        let mut stdout = Vec::new();

        let result = handler.handle(&matches, &mut client, &mut stdout).await;

        assert!(result.is_err());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn test_list_detail_empty() {
        let empty_data: Vec<libssam::PackageInfoResponse> = vec![];
        let json_result = libssam::JsonResult::success(empty_data);

        let mut client = MockClient {
            get_all_package_info_result: Some(Ok(json_result)),
            ..MockClient::new_success()
        };

        let handler = List;
        let matches = handler.command().get_matches_from(vec!["list", "--detail"]);
        let mut stdout = Vec::new();

        handler
            .handle(&matches, &mut client, &mut stdout)
            .await
            .unwrap();
        let result = String::from_utf8(stdout).unwrap();

        assert!(result.contains("No packages found"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn test_list_detail_with_packages() {
        let mut client = MockClient::new_success();
        let handler = List;
        let matches = handler.command().get_matches_from(vec!["list", "--detail"]);
        let mut stdout = Vec::new();

        handler
            .handle(&matches, &mut client, &mut stdout)
            .await
            .unwrap();
        let result = String::from_utf8(stdout).unwrap();

        assert!(result.contains("List of packages (detailed)"));
        assert!(result.contains("Package: test-pkg"));
        assert!(result.contains("Status: Running"));
        assert!(result.contains("Version: 1.0.0"));
        assert!(result.contains("Description: Test package"));
        assert!(result.contains("Package: broken-pkg"));
        assert!(result.contains("Status: Broken"));
        assert!(result.contains("Error Summary: Parse error"));
        assert!(result.contains("Error Details: Failed to parse package.toml"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn test_list_detail_error() {
        let mut client = MockClient::new_success().with_all_pkg_info_error();
        let handler = List;
        let matches = handler.command().get_matches_from(vec!["list", "--detail"]);
        let mut stdout = Vec::new();

        let result = handler.handle(&matches, &mut client, &mut stdout).await;

        assert!(result.is_err());
    }
}
