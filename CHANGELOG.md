# Changelog

<p>
  <a href="CHANGELOG.ko.md"><img src="https://img.shields.io/badge/lang-한국어-blue" alt="Korean"></a>
</p>

## [0.2.0](https://github.com/mobis-oss/ssam/compare/v0.1.0...v0.2.0) - 2026-11-04

### Breaking changes

- The SSAM package format is updated to 0.6.0.
  Packages built with `ssam-wrap` 0.1.0 (format 0.2.0) must be rebuilt.
- The Remocon API version is updated to 2.
  - RPC names and response schemas are aligned, and responses use typed protobuf messages.
  - Requests carry the API version in the `x-ssam-api-version` metadata.
  - `ssamd` serves the API on an abstract unix socket.
  - `ssamd` accepts RPC peers only from the configured groups.
- `ssam install` takes `--downgrade` in place of the force flag.
- `ssam-wrap -o/--output` is optional, and the given path is used as is.
- The package configuration is restructured.
  Bridge-only fields are grouped under `[container.network.bridge]`, and unknown fields are rejected.
- Error handling changes from fail-ignore to fail-stop across SSAM.
  An error while applying the security policy (*seccomp*, MAC), preparing data directories, or parsing configuration stops the operation.
- The minimum supported Rust version is 1.99.

### Added

- Container networking with `[container.network]`: bridge mode with port publishing, per-container internal bridge, and external network namespace mode.
- `ssam wipe-data` to clear the data of a package.
- `--version` for `ssamd`, `ssam`, and `ssam-wrap`.
- The package location (builtin, downloaded, or local file) in `ListPackages`, `ssam list`, and `ssam pkg-info`.
- More image formats as the `ssam-wrap` package filesystem source, and `--src-oci-arch` to select the source image architecture.
- `ssamd` configuration for the runtime integrity policy and the RPC peer groups.
- Architecture, guide, and reference documentation.

### Changed

- `ssamd` recovers running packages after a restart.
- `ssamd` handles package lifecycle requests in parallel.
- A package that fails to build its bundle moves to the Broken state.
- Bazel builds are pinned to Bazel 9.3.1 and take crate versions from `Cargo.toml`.
- The unused zip compression dependencies are removed.

### Security

- The signature covers the whole *dm-verity* parameter table, including the root hash.
- Signature verification reads the package file once.
- Package parsing bounds memory allocation before authentication.
- `ssamd` validates package names, data directories, and install paths against path traversal and non-regular files.
- `ssamd` hardens the overlay mount options and the *dm-verity* device names against injection.
- `ssamd` restricts the *OCI Runtime bundle* directory to its owner.
- The default *seccomp* policy blocks user namespace creation.
- The example aarch64 musl build configuration links binaries as static-pie, so their code gets ASLR.

### Fixed

- `veritysetup` arguments for paths with spaces.
- Overflow panic on an out-of-range block size in a malformed *EROFS* or *ext4* superblock.
- Loading failure on 4 KiB page kernels for packages built on a host with a larger page size.
  `ssam-wrap` builds *EROFS* images with a 4096-byte block size.
- The *project quota* kernel ABI type mismatch.
- Cleanup of device-mapper devices, package files, and volumes after failed setup or install.
- Data preservation on a failed `remove_data` upgrade.
- Concurrent bundle preparation and `wipe-data` requests.
- Handling of a closed D-Bus `ActiveState` stream.

## [0.1.0](https://github.com/mobis-oss/ssam/releases/tag/v0.1.0) - 2026-05-15

Initial public release.
