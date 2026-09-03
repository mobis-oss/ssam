// Copyright 2025-2026 Hyundai Mobis Co., Ltd.
// SPDX-License-Identifier: Apache-2.0

use std::time::Duration;

use anyhow::Context;

use libssam::remocon::{CONTROL_SOCKET_PATH, remocon_client::RemoconClient};

use tonic::transport::Endpoint;

mod client;
mod commands;

use crate::client::{GrpcClient, unix_socket_uri};
use crate::commands::Commands;

const GRPC_CONNECT_TIMEOUT: Duration = Duration::from_secs(2);

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let channel = Endpoint::from_shared(unix_socket_uri(CONTROL_SOCKET_PATH))
        .context("Error creating channel")?
        .connect_timeout(GRPC_CONNECT_TIMEOUT)
        .connect()
        .await
        .context("Error connecting to the server")?;

    let mut client = GrpcClient::new(RemoconClient::new(channel));
    let mut stdout = std::io::stdout().lock();

    Commands::run(&mut client, &mut stdout).await
}
