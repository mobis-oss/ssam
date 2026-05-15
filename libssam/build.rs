// Copyright 2025-2026 Hyundai Mobis Co., Ltd.
// SPDX-License-Identifier: Apache-2.0

use anyhow::Context;
use std::path::PathBuf;
use std::{env, fs};
use typify::{TypeSpace, TypeSpaceSettings};

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

    let schema_path = "data/remocon-responses.schema.json";
    let schema_content =
        fs::read_to_string(schema_path).with_context(|| format!("Failed to read {schema_path}"))?;
    let schema_value: serde_json::Value = serde_json::from_str(&schema_content)
        .with_context(|| format!("Failed to parse {schema_path}"))?;

    // Read schema_version from centralized $defs/schemaVersion
    let schema_version = schema_value
        .get("$defs")
        .and_then(|defs| defs.get("schemaVersion"))
        .and_then(|value| value.get("const"))
        .and_then(|value| value.as_str())
        .context("Missing schemaVersion definition in $defs")?;

    let schema: schemars::schema::RootSchema = serde_json::from_value(schema_value.clone())
        .with_context(|| format!("Failed to load RootSchema from {schema_path}"))?;

    let mut settings = TypeSpaceSettings::default();
    settings.with_replacement("Duration", "std::time::Duration", [].into_iter());
    settings.with_derive("PartialEq".to_string());

    let mut type_space = TypeSpace::new(&settings);
    type_space
        .add_root_schema(schema)
        .context("Failed to generate types from remocon schema")?;
    let generated = format!(
        "// Generated from remocon-responses.schema.json\npub const JSON_RESULT_SCHEMA_VERSION: &str = \"{}\";\n\n{}",
        schema_version,
        type_space.to_stream()
    );
    let output = PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("remocon_schema_types.rs");
    fs::write(&output, generated).context("Failed to write remocon_schema_types.rs")?;
    println!("cargo::rerun-if-changed={schema_path}");

    Ok(())
}
