// Copyright 2025-2026 Hyundai Mobis Co., Ltd.
// SPDX-License-Identifier: Apache-2.0

use anyhow::Result;
use std::path::PathBuf;
use std::{env, fs};

fn main() -> Result<()> {
    let output = PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("runtime_config_template.rs");

    fs::write(
        &output,
        format!(
            r##"pub const RUNTIME_CONFIG_TEMPLATE: &str = r#"{}"#;"##,
            serde_json::from_str::<serde_json::Value>(
                fs::read_to_string("config/runtime_template.json")?.as_ref()
            )?
        ),
    )?;

    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=config/runtime_template.json");

    Ok(())
}
