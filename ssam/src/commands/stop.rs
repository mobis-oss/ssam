// Copyright 2025-2026 Hyundai Mobis Co., Ltd.
// SPDX-License-Identifier: Apache-2.0

use std::io::Write;

use anyhow::Result;
use clap::{ArgMatches, Command};
use tonic::async_trait;

use crate::client::Client;
use crate::commands::{
    ALL_ARG, CommandHandler, PACKAGE_NAMES_ARG, dedup_names, multi_package_command,
    write_start_stop_results,
};
use libssam::{JsonResult, StartStopResult};

pub static COMMAND: Stop = Stop;

#[derive(Debug)]
pub struct Stop;

impl Stop {
    const NAME: &'static str = "stop";
}

#[async_trait(?Send)]
impl CommandHandler for Stop {
    fn name(&self) -> &'static str {
        Self::NAME
    }

    fn command(&self) -> Command {
        multi_package_command(Self::NAME, "Stops one or more packages", "Stop")
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
            writeln!(stdout, "Stopping all installed packages")?;
        } else {
            writeln!(stdout, "Stopping package(s): {}", names.join(", "))?;
        }
        let json_result = client.stop_package(names).await?;
        let results: Vec<StartStopResult> =
            JsonResult::<Vec<StartStopResult>>::parse_response(&json_result)?;
        if results.is_empty() {
            writeln!(stdout, "  No packages to stop")?;
            return Ok(());
        }
        if !write_start_stop_results(&results, stdout)? {
            anyhow::bail!("One or more packages failed to stop");
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
        assert_eq!(COMMAND.name(), Stop::NAME);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn test_stop_success() {
        let mut client = MockClient::new_success();
        let mut stdout = Vec::new();

        let handler = Stop;
        let matches = handler.command().get_matches_from(vec!["stop", "test-pkg"]);

        handler
            .handle(&matches, &mut client, &mut stdout)
            .await
            .unwrap();
        let result = String::from_utf8(stdout).unwrap();

        assert!(result.contains("Stopping package(s)"));
        assert!(result.contains("test-pkg"));
        assert!(result.contains("[OK]"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn test_stop_all() {
        let mut client = MockClient::new_success();
        let captured = client.captured_stop_names.clone();
        let mut stdout = Vec::new();

        let handler = Stop;
        let matches = handler.command().get_matches_from(vec!["stop", "--all"]);

        handler
            .handle(&matches, &mut client, &mut stdout)
            .await
            .unwrap();
        let result = String::from_utf8(stdout).unwrap();

        assert!(result.contains("Stopping all installed packages"));
        assert!(result.contains("[OK]"));

        let calls = captured.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert!(
            calls[0].is_empty(),
            "--all should send empty names to server"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn test_stop_no_args_fails() {
        let handler = Stop;
        let result = handler.command().try_get_matches_from(vec!["stop"]);
        assert!(result.is_err(), "clap should reject missing arguments");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn test_stop_error() {
        let mut client = MockClient::new_success().with_stop_error();
        let mut stdout = Vec::new();

        let handler = Stop;
        let matches = handler.command().get_matches_from(vec!["stop", "test-pkg"]);

        let result = handler.handle(&matches, &mut client, &mut stdout).await;

        assert!(result.is_err());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn test_stop_multiple_packages() {
        use libssam::StartStopResult;

        let results = vec![
            StartStopResult {
                package_name: "pkg-a".to_owned(),
                success: true,
                message: None,
            },
            StartStopResult {
                package_name: "pkg-b".to_owned(),
                success: true,
                message: None,
            },
        ];
        let mut client = MockClient::new_success().with_stop_results(results);
        let mut stdout = Vec::new();

        let handler = Stop;
        let matches = handler
            .command()
            .get_matches_from(vec!["stop", "pkg-a", "pkg-b"]);

        handler
            .handle(&matches, &mut client, &mut stdout)
            .await
            .unwrap();
        let output = String::from_utf8(stdout).unwrap();

        assert!(output.contains("Stopping package(s):"));
        assert!(output.contains("pkg-a"));
        assert!(output.contains("pkg-b"));
        assert!(output.contains("[OK] pkg-a"));
        assert!(output.contains("[OK] pkg-b"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn test_stop_partial_failure() {
        use libssam::StartStopResult;

        let results = vec![
            StartStopResult {
                package_name: "ok-pkg".to_owned(),
                success: true,
                message: None,
            },
            StartStopResult {
                package_name: "bad-pkg".to_owned(),
                success: false,
                message: Some("Package not found: bad-pkg".to_owned()),
            },
        ];
        let mut client = MockClient::new_success().with_stop_results(results);
        let mut stdout = Vec::new();

        let handler = Stop;
        let matches = handler
            .command()
            .get_matches_from(vec!["stop", "ok-pkg", "bad-pkg"]);

        let result = handler.handle(&matches, &mut client, &mut stdout).await;
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("One or more packages failed to stop")
        );

        let output = String::from_utf8(stdout).unwrap();
        assert!(output.contains("[OK] ok-pkg"));
        assert!(output.contains("[FAIL] bad-pkg"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn test_stop_all_empty_result() {
        let mut client = MockClient::new_success().with_stop_results(vec![]);
        let mut stdout = Vec::new();

        let handler = Stop;
        let matches = handler.command().get_matches_from(vec!["stop", "--all"]);

        handler
            .handle(&matches, &mut client, &mut stdout)
            .await
            .unwrap();
        let output = String::from_utf8(stdout).unwrap();

        assert!(output.contains("No packages to stop"));
    }
}
