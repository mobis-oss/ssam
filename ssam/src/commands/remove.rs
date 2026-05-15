// Copyright 2025-2026 Hyundai Mobis Co., Ltd.
// SPDX-License-Identifier: Apache-2.0

use std::io::Write;

use anyhow::Result;
use clap::{Arg, ArgMatches, Command};
use tonic::async_trait;

use crate::client::Client;
use crate::commands::CommandHandler;
use libssam::JsonResult;

pub static COMMAND: Remove = Remove;

#[derive(Debug)]
pub struct Remove;

impl Remove {
    const NAME: &'static str = "remove";
    const PACKAGE_NAME: &'static str = "package_name";
}

#[async_trait(?Send)]
impl CommandHandler for Remove {
    fn name(&self) -> &'static str {
        Self::NAME
    }

    fn command(&self) -> Command {
        Command::new(Self::NAME).about("Removes a package").arg(
            Arg::new(Self::PACKAGE_NAME)
                .help("The ID of the package to remove")
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
        let package_name = matches
            .get_one::<String>(Self::PACKAGE_NAME)
            .ok_or_else(|| anyhow::anyhow!("Package name is required"))?;

        writeln!(stdout, "Removing package: {package_name}")?;
        let json_result = client.remove_package(package_name).await?;
        JsonResult::<()>::parse_response(&json_result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::MockClient;

    #[test]
    fn test_name() {
        assert_eq!(COMMAND.name(), Remove::NAME);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn test_remove_success() {
        let mut client = MockClient::new_success();
        let mut stdout = Vec::new();

        let handler = Remove;
        let matches = handler
            .command()
            .get_matches_from(vec!["remove", "test-pkg"]);

        handler
            .handle(&matches, &mut client, &mut stdout)
            .await
            .unwrap();
        let result = String::from_utf8(stdout).unwrap();

        assert!(result.contains("Removing package"));
        assert!(result.contains("test-pkg"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn test_remove_error() {
        let mut client = MockClient::new_success().with_remove_error();
        let mut stdout = Vec::new();

        let handler = Remove;
        let matches = handler
            .command()
            .get_matches_from(vec!["remove", "test-pkg"]);

        let result = handler.handle(&matches, &mut client, &mut stdout).await;

        assert!(result.is_err());
    }
}
