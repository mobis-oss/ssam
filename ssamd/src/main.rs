// Copyright 2025-2026 Hyundai Mobis Co., Ltd.
// SPDX-License-Identifier: Apache-2.0

use std::io::{self, Write};

use anyhow::Context;

use libssam::remocon::API_VERSION;
use libssam::ssam_package::package_format_version;

fn print_version() -> anyhow::Result<()> {
    // writeln! over println! keeps the match arm in main a single expression.
    writeln!(
        io::stdout(),
        "Daemon version:           {}\n\
         RPC API version:          {API_VERSION}\n\
         Package format:           {}\n\
         OCI Runtime Spec Version: {}",
        env!("CARGO_PKG_VERSION"),
        package_format_version(),
        oci_spec::runtime::VERSION,
    )
    .context("Failed to write version information")
}

fn main() -> anyhow::Result<()> {
    match std::env::args().nth(1).as_deref() {
        Some("--version" | "-V") => print_version(),
        _ => ssamd::main(),
    }
}
