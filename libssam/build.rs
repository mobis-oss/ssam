// Copyright 2025-2026 Hyundai Mobis Co., Ltd.
// SPDX-License-Identifier: Apache-2.0

use anyhow::Context;
use std::path::PathBuf;
use std::{env, fs};
static SSAM_PROTO_FILE: &str = "data/ssam.remocon.proto";

fn main() -> anyhow::Result<()> {
    tonic_prost_build::compile_protos(SSAM_PROTO_FILE)
        .with_context(|| format!("Failed to compile {SSAM_PROTO_FILE}"))?;
    println!("cargo::rerun-if-changed={SSAM_PROTO_FILE}");

    // Generate default seccomp policy as a constant
    let output = PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("default_seccomp_policy.rs");
    let seccomp_content = fs::read_to_string("data/default_seccomp.json")
        .context("Failed to read default_seccomp.json")?;
    // Validate JSON format
    serde_json::from_str::<serde_json::Value>(&seccomp_content)
        .context("Invalid JSON in default_seccomp.json")?;
    fs::write(
        &output,
        format!(
            r##"/// Default seccomp policy for container security.
/// This policy defines system call filtering rules based on Docker's default seccomp profile.
pub const DEFAULT_SECCOMP_POLICY: &str = r#"{seccomp_content}"#;"##
        ),
    )
    .context("Failed to write default_seccomp_policy.rs")?;
    println!("cargo::rerun-if-changed=data/default_seccomp.json");

    Ok(())
}
