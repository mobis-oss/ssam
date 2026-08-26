// Copyright 2026 Hyundai Mobis Co., Ltd.
// SPDX-License-Identifier: Apache-2.0

use std::io::Write;

use anyhow::Result;
use clap::{ArgMatches, Command};
use tonic::async_trait;

use crate::client::Client;
use crate::commands::{
    ALL_ARG, CommandHandler, PACKAGE_NAMES_ARG, dedup_names, multi_package_command,
    write_package_op_results,
};

pub static COMMAND: WipeData = WipeData;

#[derive(Debug)]
pub struct WipeData;

impl WipeData {
    const NAME: &'static str = "wipe-data";
}

#[async_trait(?Send)]
impl CommandHandler for WipeData {
    fn name(&self) -> &'static str {
        Self::NAME
    }

    fn command(&self) -> Command {
        multi_package_command(
            Self::NAME,
            "Wipes the data volumes of one or more packages, keeping them installed",
            "wipe data for",
        )
    }

    async fn handle(
        &self,
        matches: &ArgMatches,
        client: &mut dyn Client,
        stdout: &mut dyn Write,
    ) -> Result<()> {
        let all = matches.get_flag(ALL_ARG);
        let names: Vec<String> = matches
            .get_many::<String>(PACKAGE_NAMES_ARG)
            .unwrap_or_default()
            .map(ToOwned::to_owned)
            .collect();
        let names = dedup_names(names);
        if all {
            writeln!(stdout, "Wiping data for all installed packages")?;
        } else {
            writeln!(stdout, "Wiping data for package(s): {}", names.join(", "))?;
        }
        let results = client.wipe_package_data(names).await?.results;
        if results.is_empty() {
            writeln!(stdout, "  No packages to wipe")?;
            return Ok(());
        }
        if !write_package_op_results(&results, stdout)? {
            anyhow::bail!("One or more packages failed to wipe data");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::MockClient;
    use libssam::PackageActionResult;

    #[test]
    fn test_name() {
        assert_eq!(COMMAND.name(), WipeData::NAME);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn test_wipe_data_success() {
        let mut client = MockClient::new_success();
        let mut stdout = Vec::new();

        let handler = WipeData;
        let matches = handler
            .command()
            .get_matches_from(vec!["wipe-data", "test-pkg"]);

        handler
            .handle(&matches, &mut client, &mut stdout)
            .await
            .unwrap();
        let result = String::from_utf8(stdout).unwrap();

        assert!(result.contains("Wiping data for package(s)"));
        assert!(result.contains("test-pkg"));
        assert!(result.contains("[OK]"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn test_wipe_data_all() {
        let mut client = MockClient::new_success();
        let captured = client.captured_wipe_names.clone();
        let mut stdout = Vec::new();

        let handler = WipeData;
        let matches = handler
            .command()
            .get_matches_from(vec!["wipe-data", "--all"]);

        handler
            .handle(&matches, &mut client, &mut stdout)
            .await
            .unwrap();
        let result = String::from_utf8(stdout).unwrap();

        assert!(result.contains("Wiping data for all installed packages"));

        let calls = captured.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert!(
            calls[0].is_empty(),
            "--all should send empty names to server"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn test_wipe_data_no_args_fails() {
        let handler = WipeData;
        let result = handler.command().try_get_matches_from(vec!["wipe-data"]);
        assert!(result.is_err(), "clap should reject missing arguments");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn test_wipe_data_multiple_packages() {
        let results = vec![
            PackageActionResult {
                package_name: "pkg-a".to_owned(),
                success: true,
                message: None,
            },
            PackageActionResult {
                package_name: "pkg-b".to_owned(),
                success: true,
                message: None,
            },
        ];
        let mut client = MockClient::new_success().with_wipe_data_results(results);
        let mut stdout = Vec::new();

        let handler = WipeData;
        let matches = handler
            .command()
            .get_matches_from(vec!["wipe-data", "pkg-a", "pkg-b"]);

        handler
            .handle(&matches, &mut client, &mut stdout)
            .await
            .unwrap();
        let output = String::from_utf8(stdout).unwrap();

        assert!(output.contains("[OK] pkg-a"));
        assert!(output.contains("[OK] pkg-b"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn test_wipe_data_partial_failure() {
        let results = vec![
            PackageActionResult {
                package_name: "ok-pkg".to_owned(),
                success: true,
                message: None,
            },
            PackageActionResult {
                package_name: "bad-pkg".to_owned(),
                success: false,
                message: Some("stop it before wiping data".to_owned()),
            },
        ];
        let mut client = MockClient::new_success().with_wipe_data_results(results);
        let mut stdout = Vec::new();

        let handler = WipeData;
        let matches = handler
            .command()
            .get_matches_from(vec!["wipe-data", "ok-pkg", "bad-pkg"]);

        let result = handler.handle(&matches, &mut client, &mut stdout).await;
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("One or more packages failed to wipe data")
        );

        let output = String::from_utf8(stdout).unwrap();
        assert!(output.contains("[OK] ok-pkg"));
        assert!(output.contains("[FAIL] bad-pkg"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn test_wipe_data_error() {
        let mut client = MockClient::new_success().with_wipe_data_error();
        let mut stdout = Vec::new();

        let handler = WipeData;
        let matches = handler
            .command()
            .get_matches_from(vec!["wipe-data", "test-pkg"]);

        let result = handler.handle(&matches, &mut client, &mut stdout).await;

        assert!(result.is_err());
    }
}
