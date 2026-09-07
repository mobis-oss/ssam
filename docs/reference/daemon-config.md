# ssamd Configuration (`ssamd.toml`) Reference

<p>
  <a href="daemon-config.ko.md"><img src="https://img.shields.io/badge/lang-한국어-blue" alt="Korean"></a>
</p>

`ssamd.toml` is the configuration file for the SSAM daemon (`ssamd`).
It configures where packages live, where their data is mounted and stored, and which public key verifies package signatures, among others.

> 📖 For a step-by-step walkthrough of installing and configuring `ssamd`, see the **[Serving SSAM guide](../guides/serving-ssam.md)**.

`ssamd` loads `ssamd.toml` from **the same directory as the `ssamd` binary**, and provides no way to override this through command-line arguments or environment variables.
Because the file sets not just the daemon's directories but also the location of the public key that verifies packages, the configuration itself is something to protect — a deliberate choice to keep the daemon from running with a tampered configuration.
So place the config file, like the daemon binary, where the system's *trust chain* protects it.

> **How to produce it:** write it by hand using this reference, or generate it with the `gen_ssamd_conf.sh` helper (bundled with the source or distribution package; see the [ssamd Operations Guide](../guides/serving-ssam.md#generating-configuration-with-gen_ssamd_confsh)).

---

## Table of Contents

1. [File structure at a glance](#file-structure-at-a-glance)
2. [`[package]`](#package)
3. [`[package.dir]`](#packagedir)
4. [`[debug]`](#debug)
5. [`[network]`](#network)
6. [`[rpc]`](#rpc)

---

## File structure at a glance

`ssamd` reads `ssamd.toml` from the directory that contains the daemon binary.
The current configuration contract is:

```toml
[package]
extension = "ssam"
cgroup = ""
public_key = "/etc/ssamd/public.key"

[package.dir]
builtin = "/usr/share/ssamd/packages"
downloaded = "/var/lib/ssamd/packages"
oci_bundle = "/var/lib/ssamd/oci-bundle"
data = "/var/lib/ssamd/data"

[debug]
pkgfs_overlay_dir = ""
disable_pkgfs_integrity = false
disable_package_verify = false

[network.bridge]
enabled = false

[network.bridge.addr_pool]
base = "172.20.0.0/16"
size = 29

[rpc]
allowed_gids = []
```

`package.dir.builtin` and `package.dir.downloaded` are optional sources. At
least one source must be non-empty. Builtin packages are read-only system
packages. Downloaded packages are the install, upgrade, and remove target.
When both sources contain a package with the same name, the downloaded package
takes precedence.

`package.dir.oci_bundle` and `package.dir.data` are required non-empty paths.
`package.dir.oci_bundle` must be an absolute path.
Package source and runtime directories must not be placed under a writable
directory shared by multiple users.
Two package source paths must not resolve to the same directory.

## `[package]`

### `extension`

Package file extension without a dot. Default: `"ssam"`.

### `cgroup`

Systemd cgroup used for package processes. Default: `""`.

### `public_key`

Path to the RSA public key used for package signature verification. Default:
`/etc/ssamd/public.key`.

The value must be non-empty when `debug.disable_package_verify` is `false`. The
daemon also requires the key file to exist before startup. An empty value is
valid only when package verification is explicitly disabled.

## `[package.dir]`

### `builtin`

Optional read-only package source. An empty value disables the source.
The directory must exist before `ssamd` starts; the daemon does not create it.

### `downloaded`

Optional read-write package source. An empty value disables the source and
causes install operations to return a configuration error.
When configured, the daemon creates the directory during startup.
`InstallPackage` requires this source, so a builtin-only deployment cannot
accept runtime package installation requests.

### `oci_bundle`

Required root for per-package OCI bundles. For package `name` and version
`version`, the daemon uses:

```text
oci_bundle/
└── name_version/
    ├── config.json
    └── pkgfs/
```

`config.json` is the OCI runtime configuration. `pkgfs/` is the package
filesystem mount target and the absolute OCI `root.path`.
The daemon creates this directory during startup.

### `data`

Required root for package container data directories.
The daemon creates this directory during startup.

## `[debug]`

### `pkgfs_overlay_dir`

Optional root for package filesystem overlay data. Empty disables the overlay.

### `disable_pkgfs_integrity`

Disables dm-verity enforcement for the package filesystem. Default: `false`.
This setting does not disable package signature verification.

### `disable_package_verify`

Disables only RSA signature verification. Default: `false`.

Package format, footer, payload bounds, metadata, runtime configuration, seccomp
policy, and filesystem payload validation still run. An invalid or malformed
package is rejected in both modes.

## `[network]`

Daemon-managed network configuration.
The whole section is optional.

### `[network.bridge]`

`enabled` controls daemon-managed bridge networking.
Its default is `false`.

### `[network.bridge.addr_pool]`

Configure the optional address pool together with its parent bridge table:

```toml
[network.bridge]
enabled = true

[network.bridge.addr_pool]
base = "172.20.0.0/16"
size = 29
```

The optional address pool defaults to `172.20.0.0/16` with `/29` per-container
subnets.
A prefix longer than `/30` is rejected because it has no usable host address.

## `[rpc]`

`allowed_gids` lists additional peer effective GIDs allowed to access the Unix
RPC socket. The daemon's effective GID is always allowed. Default: `[]`.
