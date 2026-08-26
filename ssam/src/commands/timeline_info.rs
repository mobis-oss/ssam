// Copyright 2025-2026 Hyundai Mobis Co., Ltd.
// SPDX-License-Identifier: Apache-2.0

use std::io::Write;

use anyhow::{Context, Result};
use clap::{ArgMatches, Command};
use libssam::remocon::TimelineEventKind;
use tonic::async_trait;

use crate::client::Client;
use crate::commands::CommandHandler;

pub static COMMAND: TimelineInfo = TimelineInfo;

#[derive(Debug)]
pub struct TimelineInfo;

impl TimelineInfo {
    const NAME: &'static str = "timeline-info";
}

#[async_trait(?Send)]
impl CommandHandler for TimelineInfo {
    fn name(&self) -> &'static str {
        Self::NAME
    }

    fn command(&self) -> Command {
        Command::new(Self::NAME).about("Shows timeline information")
    }

    async fn handle(
        &self,
        _matches: &ArgMatches,
        client: &mut dyn Client,
        stdout: &mut dyn Write,
    ) -> Result<()> {
        let timeline_data = client.get_timeline_info().await?;

        writeln!(stdout, "Timeline Information:")?;

        let uptime: std::time::Duration = timeline_data
            .ssamd_uptime
            .context("Timeline response missing ssamd_uptime")?
            .try_into()
            .context("Invalid ssamd_uptime duration")?;
        writeln!(stdout, "SSAMD started: {uptime:?} since system booted")?;

        for event in &timeline_data.events {
            let kind = match TimelineEventKind::try_from(event.kind) {
                Ok(TimelineEventKind::Started) => "started",
                Ok(TimelineEventKind::Completed) => "completed",
                Ok(TimelineEventKind::Unspecified) | Err(_) => "unknown",
            };
            let duration_ms = event.duration_ns / 1_000_000;
            writeln!(
                stdout,
                "{} ({}): {} at {}ms",
                event.pkg, event.phase, kind, duration_ms
            )?;
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
        assert_eq!(COMMAND.name(), TimelineInfo::NAME);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn test_timeline_info_success() {
        let mut client = MockClient::new_success();
        let handler = TimelineInfo;
        let matches = handler.command().get_matches_from(vec!["timeline-info"]);
        let mut stdout = Vec::new();

        handler
            .handle(&matches, &mut client, &mut stdout)
            .await
            .unwrap();
        let result = String::from_utf8(stdout).unwrap();

        assert_eq!(
            result,
            concat!(
                "Timeline Information:\n",
                "SSAMD started: 42s since system booted\n",
                "test-pkg (Parse): completed at 1000ms\n",
                "unknown-pkg (Unknown): unknown at 0ms\n",
            )
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn test_timeline_info_error() {
        let mut client = MockClient::new_success().with_timeline_error();
        let handler = TimelineInfo;
        let matches = handler.command().get_matches_from(vec!["timeline-info"]);
        let mut stdout = Vec::new();

        let result = handler.handle(&matches, &mut client, &mut stdout).await;

        assert!(result.is_err());
    }
}
