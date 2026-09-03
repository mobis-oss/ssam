# Writing a Remocon Client

<p>
  <a href="writing-a-remocon-client.ko.md"><img src="https://img.shields.io/badge/lang-한국어-blue" alt="Korean"></a>
</p>

This document explains how to write a client using the **Remocon** API
`ssamd` provides over gRPC. The `ssam` CLI itself is built on this protocol.
For the full protocol, see the
[Remocon gRPC Control API Reference](../reference/remocon-rpc.md).

---

## Table of Contents

1. [What you can build](#what-you-can-build)
2. [Get the protocol](#get-the-protocol)
3. [Connect to `ssamd`](#connect-to-ssamd)
4. [Send API version metadata](#send-api-version-metadata)
5. [Call a method](#call-a-method)
6. [Parse the response](#parse-the-response)
7. [Handle bulk per-package results](#handle-bulk-per-package-results)
8. [Security considerations](#security-considerations)

---

## What you can build

This RPC can be used to build clients of many shapes. For example:

- **A container manager** that owns a package's lifecycle directly — deciding
  when to install, start, stop, or remove packages based on its own policy.
- **An update manager** that owns rollout — deciding when to install a new
  version, and whether to downgrade or roll back, based on its own logic.

---

## Get the protocol

The service is defined in
[`libssam/data/ssam.remocon.proto`](../../libssam/data/ssam.remocon.proto),
proto package `ssamd.remocon`. Generate stubs for your language with whatever
gRPC/protoc tooling it uses — there's nothing SSAM-specific about the code
generation step itself. For example, this repo's own `libssam/build.rs` calls
`tonic_prost_build::compile_protos` to generate the Rust stubs; other
languages have their own equivalent (e.g. `grpc_tools.protoc` for Python,
`protoc-gen-go` for Go).

```rust
// build.rs
fn main() -> anyhow::Result<()> {
    tonic_prost_build::compile_protos("data/ssam.remocon.proto")?;
    Ok(())
}
```

---

## Connect to `ssamd`

`ssamd` accepts local gRPC connections on the fixed Linux abstract Unix domain
socket `@ssamd`. Remote TCP connections are not supported. The peer's effective
GID must match the daemon's effective GID; the daemon enforces this with
`SO_PEERCRED`.

For example, in Rust:

```rust
use tonic::transport::Endpoint;

let channel = Endpoint::from_shared("unix://\0ssamd".to_owned())?
    .connect_timeout(GRPC_CONNECT_TIMEOUT)
    .connect()
    .await?;

let mut client = RemoconClient::new(channel);
```

---

## Send API version metadata

Every Remocon request must include the `x-ssam-api-version` metadata. Its value must
match the `CURRENT` value defined in the `ApiVersion` enum in the proto file. Use the
code-generated value for this enum rather than hardcoding its numeric value.

Requests with missing or invalid `x-ssam-api-version` metadata are rejected with
gRPC `FAILED_PRECONDITION` before the RPC handler runs.

Headers rejected earlier while the client constructs metadata or by the HTTP/2
transport, such as a value containing a newline or NUL byte, do not guarantee a
`FAILED_PRECONDITION` status.

## Call a method

Pick a method from the reference's
[API at a Glance](../reference/remocon-rpc.md#api-at-a-glance) and build its
[request message](../reference/remocon-rpc.md#request-messages). For example,
[`ListPackageInfo`](../reference/remocon-rpc.md#listpackageinfo) takes a
[`ListPackageInfoRequest`](../reference/remocon-rpc.md#listpackageinforequest).
The `generated::remocon` module below is the stub module generated from the
`ssamd.remocon` proto; replace the path if your generated module uses a
different layout.

```rust
use generated::remocon::{ApiVersion, ListPackageInfoRequest};
use tonic::{metadata::MetadataValue, Request};

let mut request = Request::new(ListPackageInfoRequest {
    package_names: vec!["navi".to_string()],
});
request.metadata_mut().insert(
    "x-ssam-api-version",
    MetadataValue::try_from((ApiVersion::Current as i32).to_string())?,
);
let response = client.list_package_info(request).await?;
```

---

## Parse the response

- Each method returns a gRPC response whose payload is its declared protobuf
  response message.
- RPC failures are returned as gRPC `Status` values. Read successful response
  fields directly from the declared protobuf message. The exact fields for
  each response are listed in the reference documentation.

---

## Handle bulk per-package results

The following three methods can each take multiple packages in a single
request, and return the result for each requested package as an array:

- [`StartPackage`](../reference/remocon-rpc.md#startpackage)
- [`StopPackage`](../reference/remocon-rpc.md#stoppackage)
- [`WipePackageData`](../reference/remocon-rpc.md#wipepackagedata)

Because they return results this way, watch out for two things:
- An empty `package_names` list targets **every** installed package (see
  [Request messages](../reference/remocon-rpc.md#request-messages)). The method
  itself can't tell whether an empty list was accidental or intentional, so
  there's no built-in safeguard — add your own confirmation or safety guard
  before forwarding an empty list.
- Duplicate package names are processed only once.
- The returned [`PackageActionResult`](../reference/remocon-rpc.md#packageactionresult)
  array has one entry per package. Check each entry separately because the
  RPC can succeed while individual operations fail.

For example:

```rust
let mut request = Request::new(StopPackageRequest {
    package_names: vec!["navi".to_string(), "cluster".to_string()],
});
request.metadata_mut().insert(
    "x-ssam-api-version",
    MetadataValue::try_from((ApiVersion::Current as i32).to_string())?,
);
let response = client.stop_package(request).await?.into_inner();
for entry in response.results {
    if !entry.success {
        println!(
            "{} failed: {}",
            entry.package_name,
            entry.message.as_deref().unwrap_or("unknown error")
        );
    }
}
```

---

## Security considerations

The RPC uses a local abstract Unix domain socket and does not support remote
TCP access. The daemon checks `SO_PEERCRED` before dispatching an RPC and allows
only peers whose effective GID matches the daemon's effective GID.

- Package authenticity is enforced as a separate security layer: every
  accepted installation RPC requires verification, so an unsigned or tampered
  package is rejected at install time and never loads or runs (see
  [Control interface security](../architecture/package-integrity.md#control-interface-security-rpc)).
