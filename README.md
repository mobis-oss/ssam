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

- **Container Execution**: Runs container-native apps as *systemd* services via an OCI-compatible runtime (`crun`).
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

### 1. Build & Install

Run these commands from the root of your `ssam` repository checkout:

```console
$ SSAM_REPO=$(pwd)   # remember the repo root — later steps change directory
$ cargo install --path ssam-wrap
$ cargo install --path ssam
$ cargo install --path ssamd
```

This installs `ssam-wrap`, `ssam`, and `ssamd` into `~/.cargo/bin`.

### 2. Generate a signing key pair

Every package is signed with an *RSA* private key;
`ssamd` verifies it with the matching public key.

```console
$ mkdir -p ~/ssam/keys && cd ~/ssam/keys
$ openssl genpkey -algorithm RSA -out private_key.pem -pkeyopt rsa_keygen_bits:2048
$ openssl rsa -in private_key.pem -pubout -out public_key.pem
```

### 3. Create packages

Populate a package's filesystem either by converting a container image or by building from scratch.

**From a Docker image**

This example uses Docker's `hello-world` image.
By default `ssam-wrap` uses the `arm64` image —
on an x86-64 development machine, set [`--src-oci-arch amd64`](docs/reference/ssam-wrap.md#options).

```console
$ mkdir -p ~/ssam/{bundled,downloaded,mnt,data}
$ mkdir -p ~/ssam/docker-helloworld && cd ~/ssam/docker-helloworld
$ ssam-wrap --prepare docker-helloworld --pkgfs-src docker://docker.io/hello-world:latest .
```

Prepare mode generates the package configuration, [`config.toml`](docs/reference/package-config.md), with [`mac`](docs/reference/package-config.md#mac) turned on,
but this needs a matching *AppArmor* profile loaded on the host, so turn it off for this sample:

```diff
 [container.security]
 seccomp = true # default is true
-mac = true # default is true
+mac = false # default is true
```

Modify the Docker-derived package filesystem too, if needed, then build the package.

```console
$ mkdir -p pkgfs/etc
$ cp /etc/resolv.conf pkgfs/etc/
$ ssam-wrap --private-key ~/ssam/keys/private_key.pem --output docker-helloworld.ssam .
$ cp docker-helloworld.ssam ~/ssam/bundled/
```

**From scratch**

With no [`--pkgfs-src`](docs/reference/ssam-wrap.md#options), `ssam-wrap` leaves `pkgfs/` empty, so build your own binary into it.
For convenience, this uses a statically linked binary.

```console
$ cd ~/ssam
$ ssam-wrap --prepare helloworld helloworld/
$ cd helloworld
$ cargo new --name helloworld app/
$ cargo build --manifest-path app/Cargo.toml --target x86_64-unknown-linux-musl --release
$ cp app/target/x86_64-unknown-linux-musl/release/helloworld pkgfs/
```

Turn off *AppArmor* confinement here too:

```diff
 [container.security]
 seccomp = true # default is true
-mac = true # default is true
+mac = false # default is true
```

Point `runtime.json`'s entrypoint at the binary you just built:

```diff
     "args": [
-      "sh"
+      "/helloworld"
     ],
```

Build the package:

```console
$ ssam-wrap --private-key ~/ssam/keys/private_key.pem --output helloworld.ssam .
$ cp helloworld.ssam ~/ssam/bundled/
```

> 📖 For the full package-creation walkthrough, see [Wrapping a SSAM](docs/guides/wrapping-a-ssam.md).

### 4. Configure and start the daemon

The SSAM daemon executable, `ssamd`, reads its config file, `ssamd.toml`, from the same directory it lives in.
So generate the config file at that location with the generator tool,
then run the daemon (running it requires root).

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

> 📖 Daemon setup, *systemd* integration, and every config key are covered in [Serving SSAM](docs/guides/serving-ssam.md) and the [`ssamd.toml` reference](docs/reference/daemon-config.md).

### 5. Verify

With the daemon running, check the package status via the `ssam` CLI.

```console
$ ssam list
List of packages
Package name         Status
===============================================
docker-helloworld    Ready
helloworld           Ready
```

Packages run as *systemd* services, so check their logs with `journalctl`.

```console
$ journalctl --no-pager -u docker-helloworld
systemd[1]: Started A short description of the package.
crun[]: Hello from Docker!
crun[]: This message shows that your installation appears to be working correctly.
crun[]: To generate this message, Docker took the following steps:
crun[]:  1. The Docker client contacted the Docker daemon.
crun[]:  2. The Docker daemon pulled the "hello-world" image from the Docker Hub.
crun[]:     (amd64)
crun[]:  3. The Docker daemon created a new container from that image which runs the
crun[]:     executable that produces the output you are currently reading.
crun[]:  4. The Docker daemon streamed that output to the Docker client, which sent it
crun[]:     to your terminal.
crun[]: To try something more ambitious, you can run an Ubuntu container with:
crun[]:  $ docker run -it ubuntu bash
crun[]: Share images, automate workflows, and more with a free Docker ID:
crun[]:  https://hub.docker.com/
crun[]: For more examples and ideas, visit:
crun[]:  https://docs.docker.com/get-started/
systemd[1]: docker-helloworld.service: Deactivated successfully.

$ journalctl --no-pager -u helloworld
systemd[1]: Started A short description of the package.
crun[]: Hello, world!
systemd[1]: helloworld.service: Deactivated successfully.
```

> 📖 For how to use the `ssam` CLI, see the [`ssam` CLI Reference](docs/reference/ssam.md).

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
