// Copyright 2025-2026 Hyundai Mobis Co., Ltd.
// SPDX-License-Identifier: Apache-2.0

use std::io::{self, Write};
use std::sync::OnceLock;

use anyhow::Result;
use clap::{ArgMatches, Command};
use tonic::async_trait;

pub mod install;
pub mod list;
pub mod pkg_info;
pub mod remove;
pub mod start;
pub mod stop;
pub mod timeline_info;

use libssam::PackageInfoResponse;
use libssam::StartStopResult;

use crate::client::Client;

// Shared helpers for multi-package commands (start, stop).
// Extracted here to avoid duplication across command modules.

pub(crate) const PACKAGE_NAMES_ARG: &str = "package_names";
pub(crate) const ALL_ARG: &str = "all";

pub(crate) fn dedup_names(mut names: Vec<String>) -> Vec<String> {
    names.sort(); // dedup() only removes consecutive duplicates
    names.dedup();
    names
}

// Builds a clap Command with positional package names and --all flag.
pub(crate) fn multi_package_command(
    name: &'static str,
    about: &'static str,
    operation: &'static str,
) -> Command {
    Command::new(name)
        .about(about)
        .arg(
            clap::Arg::new(PACKAGE_NAMES_ARG)
                .help(format!("Package names to {operation}"))
                .num_args(1..)
                .required_unless_present(ALL_ARG)
                .index(1),
        )
        .arg(
            clap::Arg::new(ALL_ARG)
                .long("all")
                .action(clap::ArgAction::SetTrue)
                .help(format!("{operation} all installed packages"))
                .conflicts_with(PACKAGE_NAMES_ARG),
        )
}

pub(crate) fn write_start_stop_results(
    results: &[StartStopResult],
    stdout: &mut dyn Write,
) -> io::Result<bool> {
    let mut all_success = true;
    for op in results {
        if op.success {
            writeln!(stdout, "  [OK] {}", op.package_name)?;
        } else {
            let msg = op.message.as_deref().unwrap_or("unknown error");
            if op.package_name.is_empty() {
                writeln!(stdout, "  [FAIL] {msg}")?;
            } else {
                writeln!(stdout, "  [FAIL] {}: {msg}", op.package_name)?;
            }
            all_success = false;
        }
    }
    Ok(all_success)
}

/// Render the detail section of a `PackageInfoResponse` to the given writer.
///
/// Shared by `list --detail` and `pkg-info` to avoid duplicating the
/// formatting logic in two places.
pub(crate) fn write_package_detail(
    stdout: &mut dyn Write,
    info: &PackageInfoResponse,
) -> io::Result<()> {
    writeln!(stdout, "Info:")?;
    writeln!(stdout, "    Path: {}", info.package_path)?;
    writeln!(stdout, "    Status: {}", info.status)?;

    if info.broken {
        writeln!(stdout, "    Name: {}", info.name)?;
        if let Some(summary) = &info.error_summary {
            writeln!(stdout, "    Error Summary: {summary}")?;
        }
        if let Some(details) = &info.error_details {
            writeln!(stdout, "    Error Details: {details}")?;
        }
    } else {
        if let Some(metadata) = &info.metadata {
            writeln!(stdout, "    Version: {}", metadata.version)?;
            writeln!(stdout, "    Name: {}", metadata.package_name)?;
            writeln!(stdout, "    Description: {}", metadata.description)?;
        }

        if let Some(quota) = &info.quota {
            writeln!(stdout)?;
            writeln!(stdout, "Quota:")?;
            writeln!(stdout, "    Enabled: {}", quota.enabled)?;
            writeln!(stdout, "    Limit: {} MB", quota.limit)?;
        }
    }

    Ok(())
}

#[async_trait(?Send)]
pub trait CommandHandler: std::fmt::Debug + Sync {
    fn name(&self) -> &'static str;
    fn command(&self) -> Command;
    async fn handle(
        &self,
        matches: &ArgMatches,
        client: &mut dyn Client,
        stdout: &mut dyn Write,
    ) -> Result<()>;
}

static ALL_COMMANDS: OnceLock<Vec<&dyn CommandHandler>> = OnceLock::new();

pub struct Commands;

impl Commands {
    fn get_commands() -> &'static [&'static dyn CommandHandler] {
        ALL_COMMANDS.get_or_init(|| {
            #[cfg(test)]
            {
                vec![&tests::TEST_NOOP_COMMAND]
            }

            #[cfg(not(test))]
            {
                vec![
                    &start::COMMAND as &dyn CommandHandler,
                    &stop::COMMAND,
                    &list::COMMAND,
                    &install::COMMAND,
                    &remove::COMMAND,
                    &timeline_info::COMMAND,
                    &pkg_info::COMMAND,
                ]
            }
        })
    }

    /// Build the root CLI command
    pub fn command() -> Command {
        Command::new("ssam")
            .version("1.0")
            .about("Client tool for M.Container(Package) Daemon")
            .subcommands(Self::get_commands().iter().map(|&cmd| cmd.command()))
    }

    // Run CLI with args from environment (main entry point)
    pub async fn run(client: &mut dyn Client, stdout: &mut dyn Write) -> Result<()> {
        use std::env;
        Self::run_from(env::args_os(), client, stdout).await
    }

    // Run CLI with custom args (for testing)
    async fn run_from<I, T>(args: I, client: &mut dyn Client, stdout: &mut dyn Write) -> Result<()>
    where
        I: IntoIterator<Item = T>,
        T: Into<std::ffi::OsString> + Clone,
    {
        let matches = Self::command().get_matches_from(args);
        Self::dispatch(&matches, client, stdout).await
    }

    fn get_command(name: &str) -> Option<&'static dyn CommandHandler> {
        Self::get_commands()
            .iter()
            .find(|&&cmd| cmd.name() == name)
            .copied()
    }

    /// Internal dispatcher (private)
    async fn dispatch(
        matches: &ArgMatches,
        client: &mut dyn Client,
        stdout: &mut dyn Write,
    ) -> Result<()> {
        let (name, sub_matches) = matches
            .subcommand()
            .ok_or_else(|| anyhow::anyhow!("No subcommand provided"))?;

        let command =
            Self::get_command(name).ok_or_else(|| anyhow::anyhow!("Unknown subcommand: {name}"))?;

        command.handle(sub_matches, client, stdout).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test-only no-op command handler
    #[derive(Debug)]
    pub(super) struct TestNoopCommand;

    pub(super) static TEST_NOOP_COMMAND: TestNoopCommand = TestNoopCommand;

    #[async_trait(?Send)]
    impl CommandHandler for TestNoopCommand {
        fn name(&self) -> &'static str {
            "__test_noop__"
        }

        fn command(&self) -> Command {
            Command::new("__test_noop__").about("Test-only no-op command")
        }

        async fn handle(
            &self,
            _matches: &ArgMatches,
            _client: &mut dyn Client,
            _stdout: &mut dyn Write,
        ) -> Result<()> {
            Ok(())
        }
    }

    #[test]
    fn test_subcommands_parse() {
        let cmd = Commands::command();

        // Test that the test-only command is registered and parseable
        let matches = cmd.try_get_matches_from(["ssam", "__test_noop__"]).unwrap();
        assert!(
            matches
                .subcommand_matches(TEST_NOOP_COMMAND.name())
                .is_some()
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn test_dispatch_no_subcommand() {
        use crate::client::MockClient;
        use std::io::Cursor;

        // Create ArgMatches without any subcommand
        let cmd = Commands::command().subcommand_required(false);
        let matches = cmd
            .try_get_matches_from(["ssam"])
            .expect("Failed to parse args without subcommand");

        let mut client = MockClient::new_success();
        let mut stdout = Cursor::new(Vec::new());

        // Call dispatch - should return error because no subcommand was provided
        let result = Commands::dispatch(&matches, &mut client, &mut stdout).await;

        assert!(
            result.is_err(),
            "dispatch should return Err when no subcommand provided"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn test_dispatch_success_with_test_command() {
        use crate::client::MockClient;
        use std::io::Cursor;

        // Create ArgMatches with test-only command
        let cmd = Commands::command();
        let matches = cmd
            .try_get_matches_from(["ssam", "__test_noop__"])
            .expect("Failed to parse __test_noop__ command");

        let mut client = MockClient::new_success();
        let mut stdout = Cursor::new(Vec::new());

        // Call dispatch - should succeed with test-only command
        let result = Commands::dispatch(&matches, &mut client, &mut stdout).await;

        assert!(
            result.is_ok(),
            "dispatch should succeed with test-only command"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn test_dispatch_unknown_subcommand() {
        use crate::client::MockClient;

        let mut client = MockClient::new_success();
        let mut stdout = std::io::Cursor::new(Vec::new());

        // Create a Command with an unknown subcommand not registered in ALL_COMMANDS
        let cmd = clap::Command::new("ssam").subcommand(clap::Command::new("definitely-unknown"));

        let matches = cmd
            .try_get_matches_from(["ssam", "definitely-unknown"])
            .unwrap();

        let result = Commands::dispatch(&matches, &mut client, &mut stdout).await;
        assert!(result.is_err());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn test_run_from_success() {
        use crate::client::MockClient;

        let mut client = MockClient::new_success();
        let mut stdout = std::io::Cursor::new(Vec::new());

        let result = Commands::run_from(["ssam", "__test_noop__"], &mut client, &mut stdout).await;

        assert!(
            result.is_ok(),
            "run_from should succeed with test-only command"
        );
    }
}
