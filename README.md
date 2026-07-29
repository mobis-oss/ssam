<p align="center">
  <img src="docs/assets/ssam-logo.png" alt="SSAM Logo" width="256">
</p>

<p align="center">
  <a href="README.ko.md"><img src="https://img.shields.io/badge/lang-한국어-blue" alt="Korean"></a>
</p>

# SSAM
* Named after the Korean dish - [Ssam](https://en.wikipedia.org/wiki/Ssam), where ingredients are wrapped together.
* SSAM is a solution developed to enable fast execution of *container-native* apps and continuous runtime integrity verification in resource-constrained embedded environments such as automotive systems.
* SSAM runs container-native apps using the *OCI Runtime* standard.
* SSAM defines packages that leverage filesystem images to configure the runtime environment for container-native apps.
* SSAM provides simplified container package definition, management, and execution for embedded Linux environments.
* SSAM supports runtime integrity verification of containers through `dm-verity`.

## Background
As software integration driven by *SDV* (Software-Defined Vehicles) advances, the software complexity of automotive controllers is increasing. One of the approaches to address this growing complexity is the adoption of container technology.
However, container solutions that comply with the conventional OCI standards fall short in automotive controller environments, as they suffer from limited performance and do not adequately meet the specific requirements of automotive systems.
To address these limitations, a high-performance, security-enhanced container solution was developed — **SSAM**.

## Design principles
SSAM fixes each container's configuration at build time. A conventional OCI-based container solution, at container start, resolves the image layers and configuration to provide a writable container root and assembles the runtime environment configuration. SSAM instead builds the container root ahead of time as a single filesystem image and authors the runtime environment configuration as an *OCI Runtime config*, which it signs. This trades the flexibility of conventional OCI-based container solutions for fast start-up and runtime integrity, resting on three choices:

- **Static configuration** — the runtime configuration is declared and signed when the package is built, never mutated at runtime.
- **Continuous runtime integrity** — every filesystem block is verified on each read at runtime.
- **Single-file package** — one signed, installable unit.

## Features

- **Container Execution**: Runs container-native apps as *Systemd* services via an OCI-compatible runtime (`crun`).
- **Continuous Integrity Verification**: Verifies the package filesystem on every read at runtime with Linux `dm-verity`.
- **Package Management**: Verification, installation, removal, and upgrade of signed packages.
- **Resource Isolation**: *OCI Runtime* isolation plus *ext4* project quota for data areas shared by multiple container-native apps.

## Key Components

* [`ssamd`](ssamd): Daemon responsible for SSAM package management, container runtime environment configuration, and execution.
* [`ssam-wrap`](ssam-wrap): SSAM package creation tool with Docker (and/or OCI) image conversion support.
* [`ssam`](ssam): Command-line interface that communicates with `ssamd` over gRPC.

## Requirements

### Build requirements
- Rust 1.88.0+ (2024 edition)

### `ssamd` requirements
- *OCI Runtime*: Tested with [`crun`](https://github.com/containers/crun).
- Mandatory Linux kernel support for the following features:
  * dm-verity
  * container
  * erofs
- Optional Linux kernel support for the following features:
  * ext4 project quota
  * Bridge networking (`bridge`/`veth`/network namespaces)

### `ssam-wrap` requirements
- `lz4`
- `erofs-utils`
- `fakeroot`
- [`cryptsetup`](https://gitlab.com/cryptsetup/cryptsetup)
- For Docker image conversion:
  - [`umoci`](https://github.com/opencontainers/umoci)
  - [`skopeo`](https://github.com/containers/skopeo) >= 1.15

## Quick start

From zero to a running SSAM package on a development machine — install the tools, create a signed package, run the daemon, and verify execution.

### 1. Install the tools

Run these commands from the root of your `ssam` repository checkout:

```console
$ SSAM_REPO=$(pwd)   # remember the repo root — later steps change directory
$ cargo install --path ssam-wrap
$ cargo install --path ssam
$ cargo install --path ssamd
```

This installs `ssam-wrap`, `ssam`, and `ssamd` into `~/.cargo/bin`.

### 2. Generate a signing key pair

Every package is signed with an *RSA* private key; `ssamd` verifies it with the
matching public key.

```console
$ mkdir -p ~/ssam/keys && cd ~/ssam/keys
$ openssl genpkey -algorithm RSA -out private_key.pem -pkeyopt rsa_keygen_bits:2048
$ openssl rsa -in private_key.pem -pubout -out public_key.pem
```

### 3. Create a package

Wrap a sample app (here, the public `hello-world` image) into a signed SSAM package,
then drop it in the daemon's **bundled** packages directory. By default
`ssam-wrap` pulls the **arm64** image variant — on an x86-64 development
machine, add `--src-oci-arch amd64` or the container will fail to run
(autostart is on by default, so Step 5's Verify will show an error instead of
`Hello from Docker!`).

```console
$ mkdir -p ~/ssam/{bundled,downloaded,mnt,data}
$ mkdir -p ~/ssam/helloworld && cd ~/ssam/helloworld
$ ssam-wrap --prepare helloworld --pkgfs-src docker://docker.io/hello-world:latest .
$ ssam-wrap --private-key ~/ssam/keys/private_key.pem --output helloworld.ssam .
$ cp helloworld.ssam ~/ssam/bundled/
```

> 📖 The full walkthrough — configuring `config.toml` / `runtime.json`, building
> from a *Docker* image or from scratch — is in
> [Wrapping a SSAM](docs/guides/wrapping-a-ssam.md).

### 4. Configure and start the daemon

Generate `ssamd.toml` next to the `ssamd` binary, then start the daemon (it runs
as root and auto-loads packages from the bundled directory).

```console
$ "$SSAM_REPO"/ssamd/releasedata/gen_ssamd_conf.sh \
      --output-path ~/.cargo/bin \
      --bundled-pkgs-dir ~/ssam/bundled \
      --downloaded-pkgs-dir ~/ssam/downloaded \
      --pkgs-mnt-root ~/ssam/mnt \
      --pkgs-data-root ~/ssam/data \
      --public-key-file ~/ssam/keys/public_key.pem
$ sudo ~/.cargo/bin/ssamd
[INFO] Successfully loaded runtime configuration from '~/.cargo/bin/ssamd.toml'
```

> 📖 Daemon setup, *systemd* integration, and every config key are covered in
> [Serving SSAM](docs/guides/serving-ssam.md) and the
> [`ssamd.toml` reference](docs/reference/daemon-config.md).

### 5. Verify

With the daemon running, check the package status and its output. Packages are
run as *systemd* services, so their logs appear in `journalctl`.

```console
$ ssam list
List of packages
Package name         Status
===============================================
helloworld           Ready

$ journalctl --no-pager -u helloworld
systemd[1]: Started A short description of the package.
crun[]: Hello from Docker!
systemd[1]: helloworld.service: Deactivated successfully.
```

> 📖 Managing packages (`install` / `start` / `stop` / `remove` / `pkg-info`) is
> covered in the [`ssam` CLI Reference](docs/reference/ssam.md).

## Documentation

**Architecture**

- [Architecture Overview](docs/architecture/overview.md)
- [Execution Architecture](docs/architecture/execution.md)
- [Package Management](docs/architecture/package-management.md)
- [Package Integrity](docs/architecture/package-integrity.md)
- [Container Isolation](docs/architecture/container-isolation.md)
- [SSAM package format](docs/architecture/package-format.md)

**Guides**

- [Wrapping a SSAM](docs/guides/wrapping-a-ssam.md) — creating packages
- [Package Integrity](docs/guides/package-integrity.md) — signing, dm-verity, read-only fs
- [Container Hardening](docs/guides/container-hardening.md) — seccomp, AppArmor
- [Container Isolation](docs/guides/container-isolation.md) — network, cgroup, quota
- [Serving SSAM](docs/guides/serving-ssam.md) — running the daemon
- [Writing a Remocon Client](docs/guides/writing-a-remocon-client.md) — custom Remocon clients

**Reference**

- [`config.toml` — package configuration](docs/reference/package-config.md)
- [`ssam-wrap` command reference](docs/reference/ssam-wrap.md)
- [`ssamd.toml` — daemon configuration](docs/reference/daemon-config.md)
- [gRPC remote control API](docs/reference/remocon-rpc.md)
- [`ssam` CLI reference](docs/reference/ssam.md)

## License

Apache License 2.0 - See [LICENSE](LICENSE) for details.

Copyright 2025-2026 Hyundai Mobis Co., Ltd.
