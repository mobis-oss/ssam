# Remocon gRPC Control API Reference

<p>
  <a href="remocon-rpc.ko.md"><img src="https://img.shields.io/badge/lang-한국어-blue" alt="Korean"></a>
</p>

This document describes the Remocon API `ssamd` provides over *gRPC* to
control packages.

> 📖 For a step-by-step walkthrough of writing a Remocon client, see the
> **[Writing a Remocon Client](../guides/writing-a-remocon-client.md)** guide.

---

## Table of Contents

1. [Connection](#connection)
2. [Request metadata](#request-metadata)
3. [API at a Glance](#api-at-a-glance)
4. [Methods](#methods)
5. [Request messages](#request-messages)
6. [Response messages](#response-messages)
7. [Response data types](#response-data-types)
8. [Examples](#examples)

---

## Connection

| Property | Value |
| --- | --- |
| Transport | gRPC (HTTP/2), proto3 |
| gRPC package | `ssamd.remocon` (proto3) |
| Endpoint | Linux abstract Unix domain socket `@ssamd` |
| Auth | `SO_PEERCRED`: peer effective GID must match the daemon's effective GID or an entry in `[rpc].allowed_gids` |

Before dispatching an RPC, `ssamd` reads the peer's effective GID from
`SO_PEERCRED`. The peer is allowed when that GID matches the daemon's effective
GID or one of the additional GIDs in `[rpc].allowed_gids`; the list defaults to
empty. UID is not used for authorization.

---

## Request metadata

*gRPC* metadata are key-value pairs carried alongside a request — the equivalent of
request headers.

### `x-ssam-api-version`

**Required.** The API version the client uses. Its value must match `CURRENT` in
the `ApiVersion` enum; if it is missing or not `CURRENT`, the request is rejected
with *gRPC* `FAILED_PRECONDITION` before the RPC handler runs. Use the
code-generated enum value rather than hardcoding its numeric value.

| Value | Meaning |
| --- | --- |
| `UNSPECIFIED` | Unspecified — not a valid request version |
| `CURRENT` | The current API version — use this in requests |

> ⚠️ Headers rejected earlier while the client constructs metadata or by the
> HTTP/2 transport itself — such as values containing a newline or NUL byte — do
> not guarantee a `FAILED_PRECONDITION` status.

---

## API at a Glance

| Method | Request | Response | Response data type |
| --- | --- | --- | --- |
| [`InstallPackage`](#installpackage) | [`InstallPackageRequest`](#installpackagerequest) | [`InstallPackageResponse`](#response-messages) | [`PackageData`](#packagedata) |
| [`RemovePackage`](#removepackage) | [`RemovePackageRequest`](#removepackagerequest) | `google.protobuf.Empty` | *(none)* |
| [`StartPackage`](#startpackage) | [`StartPackageRequest`](#startpackagerequest--stoppackagerequest) | [`StartPackageResponse`](#response-messages) | array of [`PackageActionResult`](#packageactionresult) |
| [`StopPackage`](#stoppackage) | [`StopPackageRequest`](#startpackagerequest--stoppackagerequest) | [`StopPackageResponse`](#response-messages) | array of [`PackageActionResult`](#packageactionresult) |
| [`WipePackageData`](#wipepackagedata) | [`WipePackageDataRequest`](#wipepackagedatarequest) | [`WipePackageDataResponse`](#response-messages) | array of [`PackageActionResult`](#packageactionresult) |
| [`ListPackages`](#listpackages) | `google.protobuf.Empty` | [`ListPackagesResponse`](#response-messages) | array of [`PackageStatusEntry`](#packagestatusentry) |
| [`ListPackageInfo`](#listpackageinfo) | [`ListPackageInfoRequest`](#listpackageinforequest) | [`ListPackageInfoResponse`](#response-messages) | array of [`PackageInfo`](#packageinfo) |
| [`ListPackageInfoFromFiles`](#listpackageinfofromfiles) | [`ListPackageInfoFromFilesRequest`](#listpackageinfofromfilesrequest) | [`ListPackageInfoFromFilesResponse`](#response-messages) | array of [`InspectLocalPackageResult`](#inspectlocalpackageresult) |
| [`GetTimelineInfo`](#gettimelineinfo) | `google.protobuf.Empty` | [`TimelineData`](#timelinedata) | [`TimelineData`](#timelinedata) |

---

## Methods

### `InstallPackage`

Install a package from a local path.

- **Request:** [`InstallPackageRequest`](#installpackagerequest)
- **Response:** [`InstallPackageResponse`](#response-messages)
- **Response data type:** [`PackageData`](#packagedata)

Every accepted installation RPC runs full signature and integrity checks (see
[Package Integrity](../architecture/package-integrity.md)), regardless of the
`downgrade` option. A package that fails verification is rejected at this step
and is not installed (see
[Control interface security](../architecture/package-integrity.md#control-interface-security-rpc)).

Version comparison against any currently installed version of the same
package determines the outcome:

- **Same version** — always rejected, regardless of `downgrade`.
- **Older version** — rejected unless `downgrade: true` is set.
- **Newer version** — rejected if `downgrade: true` is set; use the default
  (`false`) for an upgrade.

### `RemovePackage`

Remove an installed package.

- **Request:** [`RemovePackageRequest`](#removepackagerequest)
- **Response:** `google.protobuf.Empty`
- **Response data type:** *(none)*

A missing package returns *gRPC* `NotFound`; other removal failures return
`Internal`.

### `StartPackage`

Start one or more packages.

- **Request:** [`StartPackageRequest`](#startpackagerequest--stoppackagerequest)
- **Response:** [`StartPackageResponse`](#response-messages)
- **Response data type:** array of [`PackageActionResult`](#packageactionresult)

### `StopPackage`

Stop one or more packages.

- **Request:** [`StopPackageRequest`](#startpackagerequest--stoppackagerequest)
- **Response:** [`StopPackageResponse`](#response-messages)
- **Response data type:** array of [`PackageActionResult`](#packageactionresult)

### `WipePackageData`

Clear stored data for one or more packages.

- **Request:** [`WipePackageDataRequest`](#wipepackagedatarequest)
- **Response:** [`WipePackageDataResponse`](#response-messages)
- **Response data type:** array of [`PackageActionResult`](#packageactionresult)

### `ListPackages`

List installed packages and their status, including `Broken` entries.

- **Request:** `google.protobuf.Empty`
- **Response:** [`ListPackagesResponse`](#response-messages)
- **Response data type:** array of [`PackageStatusEntry`](#packagestatusentry)

### `ListPackageInfo`

Details for one or more installed packages. An empty `package_names` list
returns all installed packages; whitespace-only names are ignored.

- **Request:** [`ListPackageInfoRequest`](#listpackageinforequest)
- **Response:** [`ListPackageInfoResponse`](#response-messages)
- **Response data type:** array of [`PackageInfo`](#packageinfo)

### `ListPackageInfoFromFiles`

Details for SSAM package files without installing them. An empty path list
returns an empty result. One result is returned per requested path in input
order; a relative path or an inspection failure is represented by a result
with `success=false`.

- **Request:** [`ListPackageInfoFromFilesRequest`](#listpackageinfofromfilesrequest)
- **Response:** [`ListPackageInfoFromFilesResponse`](#response-messages)
- **Response data type:** array of [`InspectLocalPackageResult`](#inspectlocalpackageresult)

### `GetTimelineInfo`

Daemon timeline / uptime information.

- **Request:** `google.protobuf.Empty`
- **Response:** [`TimelineData`](#timelinedata)
- **Response data type:** [`TimelineData`](#timelinedata)

---

## Request messages

`ListPackages` and `GetTimelineInfo` take
`google.protobuf.Empty` and have no request fields of their own — only the
messages below do.

### `InstallPackageRequest`

| Field | Type | Meaning |
| --- | --- | --- |
| `package_path` | `string` | Path to the SSAM package file to install |
| `downgrade` | `bool` | Install a strictly lower version than the one currently installed |
| `remove_data` | `bool` | Also clear the package's existing data on install |

### `RemovePackageRequest`

| Field | Type | Meaning |
| --- | --- | --- |
| `package_name` | `string` | Name of the installed package to remove |

### `StartPackageRequest` / `StopPackageRequest`

| Field | Type | Meaning |
| --- | --- | --- |
| `package_names` | `repeated string` | One or more packages to start/stop. When the list is empty (`[]`), it targets all installed packages. |

### `WipePackageDataRequest`

| Field | Type | Meaning |
| --- | --- | --- |
| `package_names` | `repeated string` | One or more packages whose stored data to wipe. When the list is empty (`[]`), it targets all installed packages. |

### `ListPackageInfoRequest`

| Field | Type | Meaning |
| --- | --- | --- |
| `package_names` | `repeated string` | Names of installed packages to query. An empty list means all installed packages |

### `ListPackageInfoFromFilesRequest`

| Field | Type | Meaning |
| --- | --- | --- |
| `package_paths` | `repeated string` | Absolute paths to SSAM package files to query without installing. An empty list returns an empty result |

---

## Response messages

Each RPC's response payload is its declared protobuf message.

| RPC | Response | Fields |
| --- | --- | --- |
| [`InstallPackage`](#installpackage) | `InstallPackageResponse` | `package_data`: [`PackageData`](#packagedata) |
| [`RemovePackage`](#removepackage) | `google.protobuf.Empty` | *(none)* |
| [`StartPackage`](#startpackage) | `StartPackageResponse` | `results`: repeated [`PackageActionResult`](#packageactionresult) |
| [`StopPackage`](#stoppackage) | `StopPackageResponse` | `results`: repeated [`PackageActionResult`](#packageactionresult) |
| [`WipePackageData`](#wipepackagedata) | `WipePackageDataResponse` | `results`: repeated [`PackageActionResult`](#packageactionresult) |
| [`ListPackages`](#listpackages) | `ListPackagesResponse` | `entries`: repeated [`PackageStatusEntry`](#packagestatusentry) |
| [`ListPackageInfo`](#listpackageinfo) | `ListPackageInfoResponse` | `packages`: repeated [`PackageInfo`](#packageinfo) |
| [`ListPackageInfoFromFiles`](#listpackageinfofromfiles) | `ListPackageInfoFromFilesResponse` | `packages`: repeated [`InspectLocalPackageResult`](#inspectlocalpackageresult) |
| [`GetTimelineInfo`](#gettimelineinfo) | `TimelineData` | `events`: repeated [`TimelineEvent`](#timelineevent), `ssamd_uptime`: [`google.protobuf.Duration`](#googleprotobufduration) |

---

## Response data types

The response messages use the following protobuf messages.

### `PackageStatusEntry`

| Field | Type | Meaning |
| --- | --- | --- |
| `package_name` | `string` | Installed package name |
| `status` | `string` | Package state (`Ready`, `Running`, `Broken`, …) — corresponds to the daemon's [package states](../architecture/execution.md#package-state-machine) |

### `PackageData`

Package data returned by the response.

| Field | Type | Meaning |
| --- | --- | --- |
| `package_name` | `string` | Package name |
| `version` | `string` | Package version |
| `description` | `string` | Package description |

### `PackageInfo`

Returned for each requested package by `ListPackageInfo`. `status` is the
single package status value. `success` is `false` for a missing package or a
lookup error; a `Broken` entry is returned with `success=true` and error fields.

| Field | Type | Presence | Meaning |
| --- | --- | --- | --- |
| `success` | `bool` | always | Whether package lookup succeeded |
| `name` | `string` | always | Package name |
| `status` | `string` | always | Package status |
| `package_path` | `string` | always | On-device package path; empty when unavailable |
| `package_data` | `PackageData` | optional | Package data for a normal package |
| `quota` | `QuotaInformation` | optional | Data-quota limits for a normal package |
| `error_summary` | `string` | optional | Display-level lookup or package-load error |
| `error_details` | `string` | optional | Detailed error information for Broken entries and lookup errors |

### `QuotaInformation`

| Field | Type | Meaning |
| --- | --- | --- |
| `enabled` | `bool` | Whether a data quota is enforced |
| `limit` | `uint64` | Quota limit in bytes |

### `PackageActionResult`

Per-package outcome for the bulk operations (`StartPackage`, `StopPackage`,
`WipePackageData`), which each return an **array** with one entry per package.

| Field | Type | Presence | Meaning |
| --- | --- | --- | --- |
| `package_name` | `string` | always | The package this entry is about |
| `success` | `bool` | always | Whether the operation succeeded for this package |
| `message` | `string` | optional | Detail, typically on failure |

### `InspectLocalPackageResult`

| Field | Type | Meaning |
| --- | --- | --- |
| `package_data` | `PackageData` | Package data read from the SSAM package file |
| `package_path` | `string` | Path that was queried |
| `success` | `bool` | Whether package inspection succeeded |
| `error_summary` | `string` | Display-level inspection error |
| `error_details` | `string` | Optional detailed error information |

### `TimelineData`

| Field | Type | Meaning |
| --- | --- | --- |
| `events` | `repeated TimelineEvent` | Recorded lifecycle events |
| `ssamd_uptime` | `google.protobuf.Duration` | System uptime when `ssamd` started |

### `TimelineEvent`

| Field | Type | Meaning |
| --- | --- | --- |
| `pkg` | `string` | Package name |
| `phase` | `string` | Lifecycle phase |
| `duration_ns` | `uint64` | Phase duration in nanoseconds |
| `kind` | `TimelineEventKind` | Event kind |

### `TimelineEventKind`

| Value | Number | Meaning |
| --- | --- | --- |
| `TIMELINE_EVENT_KIND_UNSPECIFIED` | `0` | No kind was set |
| `STARTED` | `1` | Phase started |
| `COMPLETED` | `2` | Phase completed |

### `google.protobuf.Duration`

| Field | Type | Meaning |
| --- | --- | --- |
| `seconds` | `int64` | Whole seconds |
| `nanos` | `int32` | Sub-second nanoseconds |

---

## Examples

The examples below are shown in protobuf **text format**, not the binary wire format actually sent over *gRPC*.

`ListPackageInfo` — success:

```text
packages {
  success: true
  name: "navi"
  status: "Running"
  package_path: "/var/lib/ssam/packages/navi"
  package_data {
    package_name: "navi"
    version: "1.4.0"
    description: "Navigation service"
  }
  quota {
    enabled: true
    limit: 536870912
  }
}
```

`StartPackage` — a successful call with a per-package failure:

```text
results {
  package_name: "navi"
  success: true
}
results {
  package_name: "cluster"
  success: false
  message: "already running"
}
```

`GetTimelineInfo` — success:

```text
events {
  pkg: "navi"
  phase: "mount"
  duration_ns: 42000000
  kind: COMPLETED
}
ssamd_uptime {
  seconds: 1234
  nanos: 500000000
}
```

In Rust with tonic, handle the typed response and optional message fields:

```rust
let response = client.list_package_info(request).await?.into_inner();
let package_info = response
    .packages
    .first()
    .context("response missing package info")?;
if package_info.success {
    let package_data = package_info
        .package_data
        .context("response missing package data")?;
} else if let Some(summary) = package_info.error_summary.as_deref() {
    println!(
        "failed: {summary}"
    );
}
```
