# Serving SSAM
#### ssamd Operations Guide

<p>
  <a href="serving-ssam.ko.md"><img src="https://img.shields.io/badge/lang-한국어-blue" alt="Korean"></a>
</p>

Once you have [wrapped a SSAM package](wrapping-a-ssam.md), **`ssamd`** is what serves it: the SSAM daemon installs packages, verifies their signatures, sets up *dm-verity* integrity checking, mounts the package filesystem, and runs the *container-native* application under *systemd*.

This guide walks you through building, deploying, configuring, and running `ssamd`.

---

## Table of Contents

1. [What ssamd does](#what-ssamd-does)
2. [Requirements](#requirements)
3. [Step 1 — Build `ssamd`](#step-1--build-ssamd)
4. [Step 2 — Prepare directories](#step-2--prepare-directories)
5. [Step 3 — Prepare the public key](#step-3--prepare-the-public-key)
6. [Step 4 — Prepare `ssamd.toml`](#step-4--prepare-ssamdtoml)
7. [Step 5 — Run under systemd](#step-5--run-under-systemd)
8. [Step 6 — Verify](#step-6--verify)
9. [Optional configuration](#optional-configuration)
10. [Generating configuration with `gen_ssamd_conf.sh`](#generating-configuration-with-gen_ssamd_confsh)
11. [Troubleshooting](#troubleshooting)

---

## What ssamd does

`ssamd` is a long-running daemon that manages the full lifecycle of SSAM packages on a device:

- **Loads & verifies** each package — opens it, parses the payloads, and checks the *RSA* signature against the configured public key.
- **Establishes integrity** by setting up a *dm-verity* target from the package's signed *dm-verity* parameters, so tampering is detected continuously at read time.
- **Mounts** the package filesystem read-only.
- **Generates the OCI Runtime config** from the package's `runtime.json`.
- **Runs** the container via an *OCI Runtime* (`crun`) as a *systemd* service, applying the package's runtime config, seccomp policy, and MAC confinement.
- **Provisions** each package's persistent data directories (with optional *ext4* project-quota limits) and network (host / none / bridge).
- **Exposes** an RPC control endpoint (the fixed abstract Unix socket `@ssamd`) that the `ssam` CLI uses to list, install, start, stop, and remove packages.

`ssamd` itself supports running as a **root**, **systemd `notify`** service.

---

## Requirements

### Build requirements

- Rust 1.88.0+ (2024 edition).

### Runtime requirements (on the target device)

**Tools:**

- [`crun`](https://github.com/containers/crun) — the *OCI Runtime* that launches containers.
- **systemd** — `ssamd` runs each container as a *systemd* service unit.

**Mandatory kernel features:**

- dm-verity
- container support (namespaces, cgroups)
- EROFS

**Optional kernel features:**

- ext4 project quota — for package `storage_limit` enforcement.
- Bridge networking (`bridge` / `veth`).

---

## Step 1 — Build `ssamd`

For a native build/install:

```console
$ cargo install --path ssamd
```

This installs the `ssamd` binary into `~/.cargo/bin`.

Or you can cross-compile.
For example, build a statically linked aarch64 binary using musl:

```console
$ cargo build -p ssamd --release --target aarch64-unknown-linux-musl
# then use target/aarch64-unknown-linux-musl/release/ssamd
```

---

## Step 2 — Prepare directories

Before configuring `ssamd`, decide and prepare the directories the daemon will use.
The paths you set here are used as-is when you generate the configuration later.

| Directory | Role | If missing |
| --- | --- | --- |
| `bundled_packages_dir` | Read-only, pre-installed packages | Daemon fails |
| `downloaded_packages_dir` | Read-write, runtime-installed packages | Auto-created |
| `packages_data_root` | Per-package persistent data (+ quota) | Auto-created |
| `packages_mnt_root` | Where verified package filesystem images are mounted | Auto-created |
| `packages_overlayfs_root` | Overlay filesystem root (optional) | Auto-created |

`bundled_packages_dir` must exist ahead of time; `ssamd` does not create it.
The rest are created on startup if missing, but **pre-creating them all is recommended**: in production these directories are typically dedicated partitions or mount points (e.g. `packages_data_root` presumes a filesystem mounted with *ext4* project quota), and if the parent path they mount onto is read-only you cannot rely on runtime auto-creation.

For example:

```console
# mkdir -p \
      /var/lib/ssamd/bundled \
      /var/lib/ssamd/downloaded \
      /var/lib/ssamd/data \
      /var/run/ssamd/mnt
```

Place packages you want pre-installed on the device into `bundled_packages_dir`.
For example, copy a SSAM package built with the [Wrapping a SSAM guide](wrapping-a-ssam.md) into this directory:

```console
# cp helloworld.ssam /var/lib/ssamd/bundled/
```

---

## Step 3 — Prepare the public key

Install the verification public key.
This is the public half of the *RSA* key pair used to sign packages with `ssam-wrap`:

```console
# mkdir -p /etc/ssamd
# cp public_key.pem /etc/ssamd/public.key
```

This public key is the only thing `ssamd` trusts to verify packages, so the entire trust model hinges on its integrity — if an attacker swaps in their own key, forged packages pass.
**Place it on a read-only/secure-boot-protected path and tightly restrict write access.**

> 📖 For key-pair generation and management and the secure-boot trust chain, see [Package Integrity → Package signing](package-integrity.md#package-signing--rsa); for the signature format, see [Package Format → Signature and integrity](../architecture/package-format.md#signature-and-integrity).

---

## Step 4 — Prepare `ssamd.toml`

`ssamd` reads `ssamd.toml` from the same directory as its binary (the only location it reads).
You can write this file by hand using the [`ssamd.toml` Configuration Reference](../reference/daemon-config.md), but for convenience SSAM ships a generator script.
Run [`gen_ssamd_conf.sh`](#generating-configuration-with-gen_ssamd_confsh) with the paths you set in the previous steps to generate a minimal configuration (the file is written to `--output-path`, and must end up next to the `ssamd` binary):

```console
$ ssamd/releasedata/gen_ssamd_conf.sh \
      --output-path . \
      --bundled-pkgs-dir /var/lib/ssamd/bundled \
      --downloaded-pkgs-dir /var/lib/ssamd/downloaded \
      --pkgs-data-root /var/lib/ssamd/data \
      --pkgs-mnt-root /var/run/ssamd/mnt \
      --public-key-file /etc/ssamd/public.key
```

The generated `ssamd.toml` looks like this, and you can also write it by hand:

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

Every `[common]` key (except `bundled_packages_dir`) must be present.

The following keys, which you can leave empty, are **optional-feature** switches; how to prepare them is described in [Optional configuration](#optional-configuration):

- `packages_cgroup` — empty places containers in no dedicated cgroup slice.
- `packages_overlayfs_root` — empty disables the overlay (the production default).

> 📖 For the meaning and defaults of each key and all other configuration, see the [`ssamd.toml` Configuration Reference](../reference/daemon-config.md); for all helper options, see [Generating configuration](#generating-configuration-with-gen_ssamd_confsh).

---

## Step 5 — Run under systemd

`ssamd` must run as **root**, and since it signals readiness through *systemd*'s `sd_notify`, running it as a *systemd* **`notify`** service is recommended where possible, though not required.

Running it directly (foreground) for a quick test:

```console
# ./ssamd
[INFO] Successfully loaded runtime configuration from '.../ssamd.toml'
...
```

As a service, use a unit of `Type=notify` that runs the binary from the directory containing `ssamd.toml`.
The [`gen_ssamd_conf.sh`](#generating-configuration-with-gen_ssamd_confsh) helper can generate a matching `ssamd.service` for you too.

---

## Step 6 — Verify

Check that the daemon started and packages are recognized:

```console
$ ssam list
List of packages
Package name         Status
===============================================
helloworld           Ready
```

Package containers run as *systemd* services, so their stdout/stderr and lifecycle appear in `journalctl` under the package's unit.
For example, a package's logs:

```console
$ journalctl --no-pager -u helloworld
systemd[1]: Started helloworld.service.
...
systemd[1]: helloworld.service: Deactivated successfully.
```

> 📖 For how to manage packages (`ssam list`/`start`/`stop`/`install`/`remove`), see the [`ssam` CLI Reference](../reference/ssam.md).

---

## Optional configuration

The following features are not required to run the daemon; you configure them to match your operational policy.
Using them needs separate host-side preparation.

### Storage quota (`storage_limit`)

To enforce a package's `container.storage_limit`, the filesystem holding `packages_data_root` must support *ext4* project quota.

> 📖 For the requirements and setup, see [Container Isolation → Storage quota](container-isolation.md#storage-quota-storage_limit).

### Container cgroup (`packages_cgroup`)

Setting `packages_cgroup` makes `ssamd` place each container under the named cgroup, which must exist before `ssamd` starts.

> 📖 For the systemd-slice and outside-systemd setups, see [Container Isolation → Container cgroup](container-isolation.md#container-cgroup-packages_cgroup).

### AppArmor confinement for `ssamd`

To confine the `ssamd` daemon itself with *AppArmor*, the host must author and load a profile attached to the daemon binary path — the `ssamd` runtime does not apply a profile to itself (the MAC it applies to *containers* is covered in [Container Hardening → Mandatory access control](container-hardening.md#mandatory-access-control--apparmor)).

Both the profile that confines the `ssamd` daemon and the `container-default` profile used as the default for the containers `ssamd` runs are, by default, **authored and loaded by the operator** on the target device.
[`gen_ssamd_conf.sh`](#generating-configuration-with-gen_ssamd_confsh)'s `--gen-apparmor-profile` generates both in one file — usable as-is, or as a **template** to modify as needed:

- **`ssamd` daemon profile** — attached to the daemon binary path; confines the daemon itself.
- **`container-default` profile** — the fallback for package containers that ship without a dedicated profile.

Load the generated profile into *AppArmor* on the target device.

### Networking

A package's `host` and `none` modes need no daemon configuration.
If a package selects **`bridge`** mode, enable the daemon bridge via `[network.bridge]` in `ssamd.toml`, and the host's `bridge` / `veth` kernel features must be enabled:

```toml
[network.bridge]
enabled = true
```

> 📖 For detailed settings, see [Container Isolation → Bridge networking](container-isolation.md#bridge-networking).

---

## Generating configuration with `gen_ssamd_conf.sh`

You can write `ssamd.toml` by hand, but `gen_ssamd_conf.sh` is provided for convenience.
`gen_ssamd_conf.sh` accepts command-line options and generates `ssamd.toml` and, on request, a *systemd* service unit and an *AppArmor* profile.
It is an optional convenience, for example, when provisioning target devices consistently.

Specify options to generate the configuration.
The table below also describes whether each option is required and what happens when it is omitted:

| Option | Purpose |
| --- | --- |
| `--output-path PATH` | (optional) Specifies where generated files are written (default: current directory). |
| `--gen-service-unit` | (optional) Generates `ssamd.service` (*systemd* service unit). |
| `--gen-apparmor-profile` | (optional) Generates an *AppArmor* profile for `ssamd`. |
| `--ssamd-install-path PATH` | (conditionally required) Specifies the `ssamd` installation path on the target device. Required when generating a service unit or an *AppArmor* profile. |
| `--pkgs-mnt-root PATH` | [`packages_mnt_root`](../reference/daemon-config.md#packages_mnt_root). (required) Specifies the root directory where package filesystems are mounted. |
| `--bundled-pkgs-dir PATH` | [`bundled_packages_dir`](../reference/daemon-config.md#bundled_packages_dir). (optional) Specifies the read-only directory for bundled packages. Omit for no bundled packages. |
| `--downloaded-pkgs-dir PATH` | [`downloaded_packages_dir`](../reference/daemon-config.md#downloaded_packages_dir). (required) Specifies the read-write directory for packages installed at runtime. |
| `--pkgs-data-root PATH` | [`packages_data_root`](../reference/daemon-config.md#packages_data_root). (required) Specifies the root directory for persistent data of package containers. |
| `--pkgs-overlayfs-root PATH` | [`packages_overlayfs_root`](../reference/daemon-config.md#packages_overlayfs_root). (optional) Specifies the OverlayFS root directory for a writable layer used in debugging and development. Omit to disable OverlayFS. |
| `--pkgs-cgroup NAME` | [`packages_cgroup`](../reference/daemon-config.md#packages_cgroup). (optional) Specifies the cgroup where package containers are placed. Omit to use the default cgroup. |
| `--public-key-file PATH` | [`public_key_file_path`](../reference/daemon-config.md#public_key_file_path). (required) Specifies the path to the public key file used to verify package signatures. |
| `--packages-ext EXT` | [`packages_ext`](../reference/daemon-config.md#packages_ext). (optional) Specifies the package file extension without a leading dot. Omit to use the default `ssam` extension. |
| `--rpc-allowed-gids LIST` | [`rpc.allowed_gids`](../reference/daemon-config.md#allowed_gids). (optional) Specifies the effective GIDs of additional Remocon clients to permit. Omit to allow no additional GIDs. |
| `--bridge-enabled BOOL` | [`network.bridge.enabled`](../reference/daemon-config.md#networkbridge). (optional) Configures the daemon to provide `bridge` networking to containers. Omit to disable bridge networking. |
| `--bridge-pool-base CIDR` | [`network.bridge.addr_pool.base`](../reference/daemon-config.md#networkbridgeaddr_pool). (optional) Specifies the base CIDR of the address pool for assigning per-bridge subnets. Omit to use the daemon default; requires `--bridge-enabled`. |
| `--bridge-pool-size BITS` | [`network.bridge.addr_pool.size`](../reference/daemon-config.md#networkbridgeaddr_pool). (optional) Specifies the prefix length of each per-bridge subnet. Omit to use the daemon default; requires `--bridge-enabled`. |
| `--disable-runtime-integrity BOOL` | [`integrity.disable_runtime_integrity`](../reference/daemon-config.md#disable_runtime_integrity). (optional) Disables the daemon's runtime integrity checks. Omit to keep the checks enabled. |

Example — generate the config, a service unit, and an AppArmor profile:

```console
$ ssamd/releasedata/gen_ssamd_conf.sh \
      --output-path . \
      --gen-service-unit \
      --gen-apparmor-profile \
      --ssamd-install-path /usr/bin/ssamd \
      --bundled-pkgs-dir /var/lib/ssamd/bundled \
      --downloaded-pkgs-dir /var/lib/ssamd/downloaded \
      --pkgs-data-root /var/lib/ssamd/data \
      --pkgs-mnt-root /var/run/ssamd/mnt \
      --public-key-file /etc/ssamd/public.key
```

Run `gen_ssamd_conf.sh --help` for the authoritative option list on your checkout.

---

## Troubleshooting

| Symptom | Likely cause / fix |
| --- | --- |
| `Failed to read config file ...` at startup | `ssamd.toml` is missing from the binary's directory or isn't readable. |
| `Failed to parse config file ...` | A required `[common]` key is missing or malformed. Check against the [reference](../reference/daemon-config.md#common). |
| `Cgroup ... does not exist` | `packages_cgroup` is set but the cgroup/slice was not created before startup. Create it (see [Optional configuration](#optional-configuration)) or leave the field empty. |
| Every package shows a signature/verification error | `public_key_file_path` does not match the key used to sign the packages. |
| A `bridge`-mode package fails with `bridge network mode requested but the daemon network is disabled` | The `[network.bridge]` section is absent or `enabled = false`; add/enable it (see [Networking](#networking)). |
| Package container does not appear in `journalctl` | Confirm the container's `service_type` and that systemd/`crun` are available on the target. |

> 📖 For the full quick-start (building the tools, generating keys, creating sample packages, and running them end to end), see the [Quick-start Guide](../../README.md#quick-start).
