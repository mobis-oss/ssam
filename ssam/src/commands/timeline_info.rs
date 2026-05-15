// Copyright 2025-2026 Hyundai Mobis Co., Ltd.
// SPDX-License-Identifier: Apache-2.0

use std::io::Write;

use anyhow::Result;
use clap::{ArgMatches, Command};
use libssam::remocon_schema::TimelineEventKind;
use tonic::async_trait;

use crate::client::Client;
use crate::commands::CommandHandler;
use libssam::{JsonResult, TimelineResponse};

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
        let json_result = client.get_timeline_info().await?;
        let timeline_data = JsonResult::<TimelineResponse>::parse_response(&json_result)?;

        writeln!(stdout, "Timeline Information:")?;

        let uptime = timeline_data.ssamd_uptime;
        writeln!(stdout, "SSAMD started: {uptime:?} since system booted")?;

        for event in &timeline_data.events {
            let kind = match &event.kind {
                TimelineEventKind::Started => "started",
                TimelineEventKind::Completed => "completed",
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

        assert!(result.contains("Timeline Information"));
        assert!(result.contains("SSAMD started"));
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
