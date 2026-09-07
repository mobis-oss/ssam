# Writing a Remocon Client

<p>
  <a href="writing-a-remocon-client.ko.md"><img src="https://img.shields.io/badge/lang-한국어-blue" alt="Korean"></a>
</p>

This document explains how to write a client using the **Remocon** API `ssamd` provides over *gRPC*.
The `ssam` CLI itself is built on this protocol.
Code examples use Rust (`tonic`).

> 📖 For the full protocol, see the **[Remocon gRPC Control API Reference](../reference/remocon-rpc.md)**.

---

## Table of Contents

1. [What you can build](#what-you-can-build)
2. [Get the protocol](#get-the-protocol)
3. [Connect to `ssamd`](#connect-to-ssamd)
4. [Call a method](#call-a-method)
5. [Parse the response](#parse-the-response)
6. [Security considerations](#security-considerations)

---

## What you can build

This RPC can be used to build clients of many shapes.
For example:

- **A container manager** that owns a package's lifecycle directly — deciding when to install, start, stop, or remove packages based on its own policy.
- **An update manager** that owns rollout — deciding when to install a new version, and whether to downgrade or roll back, based on its own logic.

---

## Get the protocol

The service is defined in [`libssam/data/ssam.remocon.proto`](../../libssam/data/ssam.remocon.proto), proto package `ssamd.remocon`.
Generate stubs for your language with whatever gRPC/protoc tooling it uses — there's nothing SSAM-specific about the code generation step itself (e.g. `grpc_tools.protoc` for Python, `protoc-gen-go` for Go).

For example, here we generate the Rust stubs with `tonic_prost_build::compile_protos`:

```rust
// build.rs
fn main() -> anyhow::Result<()> {
    tonic_prost_build::compile_protos("data/ssam.remocon.proto")?;
    Ok(())
}
```

---

## Connect to `ssamd`

`ssamd` accepts local *gRPC* connections on the fixed abstract Unix domain socket `@ssamd`.

Open a channel and build the client.
The daemon may not be up yet, so retry on connection failure:

```rust
use tonic::transport::Endpoint;
use tokio::time::{sleep, Duration};

let channel = loop {
    match Endpoint::from_shared("unix://\0ssamd".to_owned())?
        .connect_timeout(GRPC_CONNECT_TIMEOUT)
        .connect()
        .await
    {
        Ok(ch) => break ch,
        Err(e) => {
            eprintln!("connect failed, retrying in 100ms: {e}");
            sleep(Duration::from_millis(100)).await;
        }
    }
};

let mut client = RemoconClient::new(channel);
```

---

## Call a method

Call a method in these steps:

1. Pick a method from the reference's [API at a Glance](../reference/remocon-rpc.md#api-at-a-glance).
2. Fill in its [request message](../reference/remocon-rpc.md#request-messages).
   For example, [`ListPackageInfo`](../reference/remocon-rpc.md#listpackageinfo) takes a [`ListPackageInfoRequest`](../reference/remocon-rpc.md#listpackageinforequest).
3. Attach the `x-ssam-api-version` metadata to the request, set to [`CURRENT`](../reference/remocon-rpc.md#x-ssam-api-version) from the `ApiVersion` enum.
4. Call the method with the completed request.

The example below builds a request and calls the method.
It assumes you generated the code into `generated::remocon`:

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

> ⚠️ `x-ssam-api-version` value:
>
> - Use the code-generated `ApiVersion` enum value rather than hardcoding its numeric value.
> - A request whose version doesn't match the one the server requires is rejected with gRPC `FAILED_PRECONDITION`.

> ⚠️ The caller's effective GID must match the daemon's effective GID or be in the daemon's [allowed-GID list](../reference/daemon-config.md#rpc); otherwise `ssamd` rejects the RPC with gRPC `PermissionDenied` at the `SO_PEERCRED` check.

> ⚠️ A method that takes `package_names` — including `ListPackageInfo` — targets **every** installed package when this list is sent empty (`[]`).
> This is dangerous for a **destructive** method like `WipePackageData`, so add a separate confirmation or safety guard.

---

## Parse the response

Each method returns its corresponding response message.
If the call itself fails, it comes back as a *gRPC* `Status` (e.g. `NotFound`, `FAILED_PRECONDITION`); on success you read that message's fields directly.

> 📖 For the corresponding response message and its fields, see [Response messages](../reference/remocon-rpc.md#response-messages) and [Response data types](../reference/remocon-rpc.md#response-data-types).

### Single-message response

A method whose result is a single message reads its fields directly.

For example, an `InstallPackage` response carries a single [`PackageData`](../reference/remocon-rpc.md#packagedata), handled as:

```rust
let response = match client.install_package(request).await {
    Ok(resp) => resp.into_inner(),
    Err(status) => {
        eprintln!("install failed: {status}");
        return;
    }
};
if let Some(data) = response.package_data {
    println!("installed {} {}", data.package_name, data.version);
}
```

### Message-array response

A method whose result is an **array** of messages can have individual entries fail even when the call itself succeeds, so check **each entry's `success`** separately.

For example, a `ListPackageInfo` response carries an array of [`PackageInfo`](../reference/remocon-rpc.md#packageinfo), handled as:

```rust
let response = client.list_package_info(request).await?.into_inner();

for package in response.packages {
    if package.success {
        // Healthy package — read optional fields like package_data / quota.
        if let Some(data) = package.package_data {
            println!("{} {} ({})", data.package_name, data.version, package.status);
        }
    } else {
        // Lookup failed — read error_summary / error_details.
        eprintln!(
            "{}: {}",
            package.name,
            package.error_summary.as_deref().unwrap_or("unknown error")
        );
    }
}
```

---

## Security considerations

- Whether a Remocon client may use the control API is decided from the effective GIDs of `ssamd` and the Remocon client, so they must be aligned in the system's runtime configuration.
- Package authenticity is a **separate security layer**: even an install request from an allowed Remocon client always goes through the package's signature and integrity verification, so an unsigned or tampered package is rejected and never loads or runs.

> 📖 For the control interface's access control (local socket, peer credentials) and threat model, see [Control interface security](../architecture/package-integrity.md#control-interface-security-rpc).
