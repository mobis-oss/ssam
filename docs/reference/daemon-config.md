# ssamd Configuration (`ssamd.toml`) Reference

<p>
  <a href="daemon-config.ko.md"><img src="https://img.shields.io/badge/lang-한국어-blue" alt="Korean"></a>
</p>

`ssamd.toml` is the configuration file for the SSAM daemon (`ssamd`). It
configures where packages live, where their data is mounted and stored, and which
public key verifies package signatures, among others.

> For a step-by-step walkthrough of installing and configuring `ssamd`, see the
> **[Serving SSAM guide](../guides/serving-ssam.md)**. This document is the
> configuration **reference**.

`ssamd` loads `ssamd.toml` from **the same directory as the `ssamd` binary**, and
provides no way to override this through command-line arguments or environment
variables. Because the file sets not just the daemon's directories but also the
location of the public key that verifies packages, the configuration itself is
something to protect — a deliberate choice to keep the daemon from running with a
tampered configuration. So place the config file, like the daemon binary, where
the system's *trust chain* protects it.

> **How to produce it:** write it by hand using this reference, or generate it
> with the `gen_ssamd_conf.sh` helper (bundled with the source or distribution
> package; see the
> [ssamd Operations Guide](../guides/serving-ssam.md#generating-configuration-with-gen_ssamd_confsh)).

---

## Table of Contents

1. [File structure at a glance](#file-structure-at-a-glance)
2. [`[common]`](#common)
3. [`[network]`](#network)
4. [`[integrity]`](#integrity)
5. [Startup validation](#startup-validation)
6. [Complete examples](#complete-examples)

---

## File structure at a glance

```toml
[common]
bundled_packages_dir    = "/var/lib/ssamd/bundled"
downloaded_packages_dir = "/var/lib/ssamd/downloaded"
packages_data_root      = "/var/lib/ssamd/data"
packages_overlayfs_root = ""                          # may be empty
packages_mnt_root       = "/var/run/ssamd/mnt"
public_key_file_path    = "/etc/ssamd/public.key"
packages_cgroup         = ""                          # may be empty
packages_ext            = "ssam"

[network.bridge]                                      # optional section
enabled = true

[network.bridge.addr_pool]                            # optional
base = "172.20.0.0/16"                                # default
size = 29                                             # default

[integrity]                                           # optional section
disable_runtime_integrity = false                     # default
```

Every key in `[common]` **must be present** or the daemon fails to start. The
`[network]` and `[integrity]` sections are **optional**.

---

## `[common]`

Core daemon paths and settings. All keys are strings.

### `bundled_packages_dir`

- **Required.** Directory holding **bundled** (read-only, pre-installed)
  SSAM packages. Must already exist — `ssamd` aborts at startup if it
  doesn't.

### `downloaded_packages_dir`

- **Required.** Directory holding **downloaded** (read-write, installed at
  runtime) SSAM packages. Created automatically at startup if it doesn't
  exist.

### `packages_data_root`

- **Required.** Root directory under which each package's persistent,
  writable **data directories** (see the package's
  [`data_dirs`](package-config.md#data_dirs)) are stored. This is
  also where *ext4* project quota (`storage_limit`) is enforced.

### `packages_overlayfs_root`

- **Required to be present**, but **may be an empty string** (`""`).
- A convenience for development/testing: it layers a read-write overlay over the
  *dm-verity*-verified, read-only package filesystem so files can be edited in
  place. Point it at a suitable read-write location.
- The container runs on the overlay's merged view and uses whatever the upper
  layer overrides, so the integrity that *dm-verity* guarantees is effectively
  defeated. Leave it empty in production to disable it.

### `packages_mnt_root`

- **Required.** Root directory where package filesystems (`pkgfs`) are mounted
  at runtime after *dm-verity* verification.

### `public_key_file_path`

- **Required.** Path to the **RSA public key** used to verify package
  signatures. This must be the public key matching the private key passed to
  `ssam-wrap --private-key` when the package was signed — see
  [Signature and integrity](../architecture/package-format.md#signature-and-integrity).
  A package signed with a different key is rejected.
- `ssamd` **aborts at startup** if this path does not point to an existing file.

### `packages_cgroup`

- **Required to be present**, but **may be an empty string** (`""`).
- Name of the cgroup (e.g. a systemd slice like `ssamd.slice`) the package
  containers are placed under.
- `ssamd` does not create the cgroup, so if you set a value it **must already
  exist** before the daemon starts, or the daemon aborts.
- Leave it empty to skip placing containers under a specific cgroup.

### `packages_ext`

- **Required.** Package file extension, without a leading dot (e.g. `ssam`).
  Files with this extension are recognized as SSAM packages.

### RPC endpoint

The daemon's RPC control server listens on the Linux abstract Unix domain socket
`@ssamd`. The endpoint is fixed and is not configurable. The abstract socket is
not a filesystem path, so it does not create a socket file or require a runtime
directory. RPC clients must run in the same system environment and pass the
peer-credential check: the peer's effective GID must match the daemon's
effective GID. UID is not considered.

---

## `[network]`

Daemon-managed **bridge network** configuration. The whole section is optional
— omit it entirely to run without a managed bridge.

This section affects **only** the network setup of packages whose network mode
is `bridge` ([`[container.network] mode = "bridge"`](package-config.md#containernetwork)).
Packages using `host` or `none` networking work regardless of this section. A
package that requests `bridge` while the daemon bridge is disabled is **refused**
— keeping a misconfiguration from silently looking like it works.

Each bridge is **internal**: containers on it reach each other but have no
gateway, NAT, or external routing. The daemon names the bridge interface itself.

### `[network.bridge]`

- **`enabled`** — **boolean, required.** Whether the daemon-managed bridge
  network is enabled.

### `[network.bridge.addr_pool]`

Optional. Defines the address pool that per-bridge subnets are carved from. Omit
it to accept the defaults.

- **`base`** — **string, optional.** Pool base CIDR in IPv4 notation (e.g.
  `172.20.0.0/16`). Default: `172.20.0.0/16`.
- **`size`** — **integer, optional.** Per-bridge subnet prefix length; must be
  `/30` or larger (numerically ≤ 30). Default: `29`.

> Bridge networking requires the bridge / veth / network-namespace kernel
> features on the host. See the
> [Operations Guide](../guides/serving-ssam.md#networking-bridge-mode).

---

## `[integrity]`

Runtime package-filesystem integrity. The whole section is **optional** — omit it
to keep runtime integrity **enabled** (the default).

### `disable_runtime_integrity`

- **boolean, optional (default `false`).** When `true`, the daemon skips kernel
  *dm-verity* block verification at runtime — the package filesystem is mounted
  without the verity target. **Package signature verification still applies**
  regardless of this setting; only the per-read block verification is turned off.
  Leave it `false` (or omit the section) to keep continuous integrity. See
  [Package Integrity](../architecture/package-integrity.md#filesystem-integrity--dm-verity).

---

## Startup validation

At startup `ssamd`:

- **requires** `ssamd.toml` in the same location as the binary, with all mandatory
  `[common]` keys present — otherwise it fails to start with a configuration error.
- **requires** the directory set in `bundled_packages_dir` to already exist
  (aborting if not).
- **requires** the file set in `public_key_file_path` to exist, aborting with
  *"Public key file not found"* otherwise.
- **creates** the `downloaded_packages_dir`, `packages_data_root`, and
  `packages_mnt_root` directories automatically if missing.
- **checks** that `packages_cgroup` exists (when non-empty), aborting if not.
- **binds** the RPC server to the fixed abstract Unix socket `@ssamd`.
- calls `sd_notify` after the listener is bound; if `ssamd` is configured as a
  systemd `notify`-type service unit, this **signals readiness**.

---

## Complete examples

### Minimal (no managed network)

```toml
[common]
bundled_packages_dir    = "/var/lib/ssamd/bundled"
downloaded_packages_dir = "/var/lib/ssamd/downloaded"
packages_data_root      = "/var/lib/ssamd/data"
packages_overlayfs_root = ""
packages_mnt_root       = "/var/run/ssamd/mnt"
public_key_file_path    = "/etc/ssamd/public.key"
packages_cgroup         = ""
packages_ext            = "ssam"
```

### With cgroup confinement and bridge networking

```toml
[common]
bundled_packages_dir    = "/var/lib/ssamd/bundled"
downloaded_packages_dir = "/var/lib/ssamd/downloaded"
packages_data_root      = "/var/lib/ssamd/data"
packages_overlayfs_root = "/var/lib/ssamd/data/overlayfs"
packages_mnt_root       = "/var/run/ssamd/mnt"
public_key_file_path    = "/etc/ssamd/public.key"
packages_cgroup         = "ssamd.slice"
packages_ext            = "ssam"

[network.bridge]
enabled = true

[network.bridge.addr_pool]
base = "172.20.0.0/16"
size = 29
```
