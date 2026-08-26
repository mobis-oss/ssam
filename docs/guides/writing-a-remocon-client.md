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
4. [Call a method](#call-a-method)
5. [Parse the response](#parse-the-response)
6. [Handle bulk per-package results](#handle-bulk-per-package-results)
7. [Security considerations](#security-considerations)

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

- **Endpoint:** `http://<host>:<port>` —
  [plain HTTP/2, no TLS](../reference/remocon-rpc.md#connection).
- **`<host>`:** the daemon's
  [`rpc_bind_ip`](../reference/daemon-config.md#rpc_bind_ip) (`127.0.0.1` by
  default).
- **`<port>`:** fixed at **63737**.
- **TLS:** none — most gRPC client libraries need an explicit `insecure` or
  `plaintext` channel option (e.g. Java's `usePlaintext()`, Python's
  `grpc.insecure_channel`). Rust's `tonic` infers this from the plain
  `http://` scheme, so no extra flag is needed — check your library's docs
  for the equivalent.

For example, in Rust:

```rust
let channel = Channel::from_shared(format!("http://{ip}:{CONTROL_PORT}"))?
    .connect_timeout(GRPC_CONNECT_TIMEOUT)
    .connect()
    .await?;

let mut client = RemoconClient::new(channel);
```

---

## Call a method

Pick a method from the reference's
[API at a Glance](../reference/remocon-rpc.md#api-at-a-glance) and build its
[request message](../reference/remocon-rpc.md#request-messages). For example,
[`ListPackageInfo`](../reference/remocon-rpc.md#listpackageinfo) takes a
[`ListPackageInfoRequest`](../reference/remocon-rpc.md#listpackageinforequest):

```rust
let request = ListPackageInfoRequest {
    package_names: vec!["navi".to_string()],
};
let response = client.list_package_info(request).await?;
```

---

## Parse the response

- Every method returns a single `result: string` field, written in the same
  JSON format.
- If the method call succeeds, `success` is `true` and `data` is present;
  if it fails, `success` is `false` and `reason` is present. An RPC that
  returns no data, such as `RemovePackage`, omits `data` even on success.
- The `data` field is defined differently for each message.
- The exact shape of `data` per message is defined in
  [`libssam/data/remocon-responses.schema.json`](../../libssam/data/remocon-responses.schema.json)
  (JSON Schema).
- For the exact response message and response data payload per method, see
  the reference's [API at a Glance](../reference/remocon-rpc.md#api-at-a-glance).
- Process the response message in the following order:
  1. Check `schema_version` first. Reject a version you don't recognize
     rather than guess at the shape of `data`.
  2. Check `success`. On failure, read `reason`. On success, decode `data`
     as the method's
     [response data payload](../reference/remocon-rpc.md#response-data-payload).

For example, handle the response for the `ListPackageInfo` method like this:

```rust
#[derive(serde::Deserialize)]
struct PackageInfo {
    success: bool,
    name: String,
    status: String,
    package_path: String,
}

let json_result = response.into_inner().result;
let parsed: serde_json::Value = serde_json::from_str(&json_result)?;
if parsed["success"] == true {
    let packages: Vec<PackageInfo> = serde_json::from_value(parsed["data"].clone())?;
    for package in packages {
        println!(
            "{}: {} (success={}, path={})",
            package.name, package.status, package.success, package.package_path
        );
    }
} else {
    println!("failed: {}", parsed["reason"]);
}
```

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
- The returned [`packageActionResult`](../reference/remocon-rpc.md#packageactionresult)
  array has one entry per package. The response's own `success` can be
  `true` even when individual entries failed, so check each entry
  separately.

For example:

```rust
let request = StopPackageRequest {
    package_names: vec!["navi".to_string(), "cluster".to_string()],
};
let response = client.stop_package(request).await?;
let json_result = response.into_inner().result;
let parsed: serde_json::Value = serde_json::from_str(&json_result)?;
if parsed["success"] == true {
    for entry in parsed["data"].as_array().unwrap() {
        if entry["success"] != true {
            println!("{} failed: {}", entry["package_name"], entry["message"]);
        }
    }
}
```

---

## Security considerations

The RPC has no TLS or authentication of its own (see
[Connection](../reference/remocon-rpc.md#connection)).

- Keep the daemon's default localhost binding unless you specifically need
  remote access, and if you do expose it, restrict access with network or
  OS-level controls (see
  [`rpc_bind_ip`](../reference/daemon-config.md#rpc_bind_ip)).
- This doesn't weaken package authenticity: verification is enforced
  independently of who calls the RPC, so an unsigned or tampered package is
  rejected at install time and never loads or runs (see
  [Control interface security](../architecture/package-integrity.md#control-interface-security-rpc)).
