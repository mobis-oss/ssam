# ssam-wrap Command Reference

<p>
  <a href="ssam-wrap.ko.md"><img src="https://img.shields.io/badge/lang-한국어-blue" alt="Korean"></a>
</p>

`ssam-wrap` is the SSAM package creation tool. It takes a *container-native*
application's filesystem — supplied either as a directory, a pre-built image
file, or a Docker/OCI container image — and produces a signed,
integrity-protected SSAM package that `ssamd` can install and run as a
container.

> For a step-by-step walkthrough of creating a package, see the
> **[Wrapping a SSAM — Package Creation Guide](../guides/wrapping-a-ssam.md)**.
> This document is the command-line **reference**: options, modes, and behavior.

---

## Table of Contents

1. [Requirements](#requirements)
2. [Modes](#modes)
3. [Command-line Reference](#command-line-reference)
4. [The Workspace](#the-workspace)
5. [Package Filesystem Types](#package-filesystem-types)
6. [Signing](#signing)
7. [Troubleshooting](#troubleshooting)

---

## Requirements

`ssam-wrap` shells out to a number of external tools. Make sure the following
are installed and available on `PATH`:

| Tool | Purpose | Required when |
| --- | --- | --- |
| `erofs-utils` (`mkfs.erofs`) | Build EROFS package filesystem images | When building EROFS (default type) |
| `lz4` | LZ4 compression backend for EROFS | Building `erofs-lz4` / `erofs-lz4hc` |
| `e2fsprogs` (`mkfs.ext4`) | Build ext4 package filesystem images | Using `-t ext4` |
| `fakeroot` | Build images without root privileges | Building any image from a directory |
| `cryptsetup` (`veritysetup`) | Generate the dm-verity hash tree | Always (wrap mode) |
| `truncate` | Pre-size ext4 images | Using `-t ext4` |
| [`skopeo`](https://github.com/containers/skopeo) (>= 1.15) | Copy container images | Docker/OCI source |
| [`umoci`](https://github.com/opencontainers/umoci) | Unpack OCI images to a rootfs | Docker/OCI source |
| `docker` | Inspect image architecture | `docker-daemon:` source only |

> **Note:** `truncate` is part of GNU coreutils and is normally already
> present on Linux systems.

---

## Modes

`ssam-wrap` operates in one of two mutually exclusive modes:

- **Prepare mode** (`--prepare`) — scaffolds template config files (`config.toml`,
  `runtime.json`, `seccomp.json`) in the workspace. Never overwrites existing files.
- **Wrap mode** (default, requires `--private-key`) — builds the package filesystem
  image, computes the *dm-verity* hash tree, and signs it into the final SSAM
  package file.

For each step in detail and how to use them, see the
[Package Creation Guide](../guides/wrapping-a-ssam.md).

---

## Command-line Reference

```
ssam-wrap [OPTIONS] <WORKSPACE>
```

### Positional argument

| Argument | Description |
| --- | --- |
| `<WORKSPACE>` | **(required)** Path to the workspace directory. Created if it does not exist. Relative paths are resolved against the current working directory. |

### Options

| Option | Long form | Wrap-only | Description |
| --- | --- | --- | --- |
| `-p <PACKAGE_NAME>` | `--prepare <PACKAGE_NAME>` | — | Run in **prepare mode**: generate `config.toml`, `runtime.json`, and `seccomp.json` scaffolding for a package of that name. |
| `-s <SRC>` | `--pkgfs-src <SRC>` | — | Package filesystem source. Either a local path (directory or image file) or a supported container transport URI (see below). |
| `-a <ARCH>` | `--src-oci-arch <ARCH>` | — | Source image architecture for container transports: `arm64` (default) or `amd64`. Ignored for non-container sources. |
| `-k <FILE>` | `--private-key <FILE>` | ✓ | RSA private key used to sign the package. **Required in wrap mode.** |
| `-o <FILE>` | `--output <FILE>` | ✓ | Output package path. Defaults to `<WORKSPACE>/<name>-<version>.ssam`. |
| `-t <TYPE>` | `--pkgfs-type <TYPE>` | ✓ | Package filesystem image type. Only used when the source is a directory. One of `ext4`, `erofs`, `erofs-lz4`, `erofs-lz4hc` (default). |

> **Wrap-only** (✓) options passed together with `--prepare` are an error.

### Supported container transports

`--pkgfs-src` accepts a subset of [skopeo transports](https://github.com/containers/image/blob/main/docs/containers-transports.5.md):

- `docker://` — a remote registry image (e.g. `docker://docker.io/hello-world:latest`)
- `docker-daemon:` — an image from the local *Docker* daemon
- `docker-archive:` — a `docker save` tar archive
- `oci-archive:` — an OCI image tar archive
- `oci:` — an OCI image layout directory

Anything else passed to `--pkgfs-src` is treated as a **local filesystem path**.

---

## The Workspace

A workspace is just a directory. After preparing and building a package it
contains:

```
<workspace>/
├── config.toml        # package configuration (metadata, security, service)
├── runtime.json       # OCI Runtime spec (process args, mounts, capabilities)
├── seccomp.json       # seccomp policy
├── pkgfs/             # package filesystem: directory, symlink, or rootfs
├── intermediate/      # build artifacts (fs image, root hash) — safe to delete
└── <name>-<version>.ssam   # the final package (default output location)
```

- If `pkgfs/` is a **directory**, `ssam-wrap` builds a filesystem image from it.
- If `pkgfs/` (or its symlink target) is a **file**, it is treated as an
  already-built image and used as-is (the `--pkgfs-type` option is ignored).
- The `intermediate/` directory holds the built image and the `.root_hash`
  file. It can be deleted between builds.

For what the three config files contain and how to fill them in, see the
[Package Creation Guide](../guides/wrapping-a-ssam.md) and the
[`config.toml` Reference](package-config.md).

---

## Package Filesystem Types

Selected with `-t` / `--pkgfs-type` (only when the source is a directory):

| Type | Backend | Notes |
| --- | --- | --- |
| `erofs-lz4hc` | `mkfs.erofs -zlz4hc` | **Default.** Highest compression, read-optimized. |
| `erofs-lz4` | `mkfs.erofs -zlz4` | Faster build, lower compression than lz4hc. |
| `erofs` | `mkfs.erofs` | Uncompressed EROFS. |
| `ext4` | `mkfs.ext4` | Writable-style layout; image is pre-sized with `truncate` (source size + 10% overhead, minimum 3 MiB). |

*EROFS* is recommended for SSAM's read-only, integrity-verified runtime model.

---

## Signing

Every package is signed with an *RSA* private key; `ssamd` verifies it against
the corresponding public key. Generate a 2048-bit key pair with OpenSSL:

```console
$ openssl genpkey -algorithm RSA -out private_key.pem -pkeyopt rsa_keygen_bits:2048
$ openssl rsa -in private_key.pem -pubout -out public_key.pem
```

- Pass `private_key.pem` to `ssam-wrap` via `--private-key`.
- Configure `ssamd` with the matching `public_key.pem`
  ([`public_key_file_path`](daemon-config.md#public_key_file_path) in `ssamd.toml`).

On success, `ssam-wrap` prints the package name, service type, the signing key
used, and the signed dm-verity table.

---

## Troubleshooting

| Symptom | Likely cause / fix |
| --- | --- |
| `Private key file path is required` | You ran wrap mode without `--private-key`. Add `-k <key>`, or use `--prepare` if you only want to scaffold. |
| `Package filesystem already exists, please delete ...` | A previous Docker/OCI extraction left a `pkgfs/`. Remove it before re-extracting. |
| `Required definition of mount '/dev' is missing ...` | `runtime.json` lacks a mandatory mount. Add `/dev`, `/dev/pts` (and `/run` for `notify` services). |
| `Docker image ... is not for <arch> architecture` | The `docker-daemon:` image's architecture differs from `--src-oci-arch`. Pass the correct `-a` value. |
| `skopeo` / `umoci` not found | Install the container tools (see [Requirements](#requirements)). |
| `WARN: Given package filesystem type would be ignored!` | The source resolved to an image file, not a directory; `-t` only applies to directory sources. |

For the full package-creation walkthrough, see the
[Wrapping a SSAM guide](../guides/wrapping-a-ssam.md). For a complete end-to-end example
including running the package as a container under `ssamd`, see the
[Quick-start Guide](../quick-start.md).
