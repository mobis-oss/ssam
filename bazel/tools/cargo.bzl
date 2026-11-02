"""Cargo helpers shared by the crate packages."""

load("@rules_rust//cargo:defs.bzl", "cargo_toml_env_vars")

def crate_cargo_env_vars(name = "cargo_env_vars"):
    """Generates the CARGO_PKG_* env vars of the package's Cargo.toml.

    Pass the target to `rustc_env_files` so Bazel builds see the same
    values as Cargo, including those inherited from the workspace.

    Args:
        name: Name of the generated env file target.
    """
    cargo_toml_env_vars(
        name = name,
        src = "Cargo.toml",
        workspace = "//:Cargo.toml",
    )
