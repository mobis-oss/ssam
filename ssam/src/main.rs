// Copyright 2025-2026 Hyundai Mobis Co., Ltd.
// SPDX-License-Identifier: Apache-2.0

use std::env;
use std::net::Ipv4Addr;
use std::time::Duration;

use anyhow::Context;

use libssam::remocon::remocon_client::RemoconClient;

use tonic::transport::Channel;

mod client;
mod commands;

use crate::client::GrpcClient;
use crate::commands::Commands;

const GRPC_CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
use libssam::remocon::CONTROL_PORT;

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let ip: Ipv4Addr = match env::var("SSAMD_IP") {
        Ok(val) => val
            .parse::<Ipv4Addr>()
            .with_context(|| format!("invalid SSAMD_IP value: '{val}'"))?,
        Err(_) => Ipv4Addr::LOCALHOST,
    };

    let channel = Channel::from_shared(format!("http://{ip}:{CONTROL_PORT}"))
        .context("Error creating channel")?
        .connect_timeout(GRPC_CONNECT_TIMEOUT)
        .connect()
        .await
        .context("Error connecting to the server")?;

    let mut client = GrpcClient::new(RemoconClient::new(channel), ip);
    let mut stdout = std::io::stdout().lock();

    Commands::run(&mut client, &mut stdout).await
}
