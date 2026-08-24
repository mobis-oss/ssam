# Remocon gRPC Control API Reference

<p>
  <a href="remocon-rpc.ko.md"><img src="https://img.shields.io/badge/lang-한국어-blue" alt="Korean"></a>
</p>

This document describes the Remocon API `ssamd` provides over *gRPC* to
control packages.

---

## Table of Contents

1. [Connection](#connection)
2. [API at a Glance](#api-at-a-glance)
3. [Methods](#methods)
4. [Request messages](#request-messages)
5. [Response messages](#response-messages)
6. [Response data payload](#response-data-payload)
7. [Examples](#examples)

---

## Connection

| Property | Value |
| --- | --- |
| Transport | gRPC (HTTP/2), proto3 |
| gRPC package | `ssamd.remocon` (proto3) |
| Host | [`rpc_bind_ip`](daemon-config.md#rpc_bind_ip) — `127.0.0.1` by default |
| Port | **63737** (fixed SSAM control port) |
| Auth | None at the transport/RPC level |

---

## API at a Glance

| Method | Request | Response | Response data payload |
| --- | --- | --- | --- |
| [`InstallPackage`](#installpackage) | [`InstallPackageRequest`](#installpackagerequest) | [`InstallPackageResponse`](#installpackageresponse) | [`installResponse`](#installresponse) |
| [`RemovePackage`](#removepackage) | [`RemovePackageRequest`](#removepackagerequest) | [`RemovePackageResponse`](#removepackageresponse) | *(none)* |
| [`StartPackage`](#startpackage) | [`StartPackageRequest`](#startpackagerequest--stoppackagerequest) | [`StartPackageResponse`](#startpackageresponse) | array of [`packageOpResult`](#packageopresult) |
| [`StopPackage`](#stoppackage) | [`StopPackageRequest`](#startpackagerequest--stoppackagerequest) | [`StopPackageResponse`](#stoppackageresponse) | array of [`packageOpResult`](#packageopresult) |
| [`WipePackageData`](#wipepackagedata) | [`WipePackageDataRequest`](#wipepackagedatarequest) | [`WipePackageDataResponse`](#wipepackagedataresponse) | array of [`packageOpResult`](#packageopresult) |
| [`ListPackagesStatus`](#listpackagesstatus) | `google.protobuf.Empty` | [`ListPackagesStatusResponse`](#listpackagesstatusresponse) | array of [`packageStatusEntry`](#packagestatusentry) |
| [`GetPackageInfo`](#getpackageinfo) | [`PackageInformationRequest`](#packageinformationrequest) | [`PackageInformationResponse`](#packageinformationresponse) | [`packageInfoResponse`](#packageinforesponse) |
| [`GetAllPackageInfo`](#getallpackageinfo) | `google.protobuf.Empty` | [`AllPackageInformationResponse`](#allpackageinformationresponse) | array of [`packageInfoResponse`](#packageinforesponse) |
| [`InspectLocalPackage`](#inspectlocalpackage) | [`InspectLocalPackageRequest`](#inspectlocalpackagerequest) | [`InspectLocalPackageResponse`](#inspectlocalpackageresponse) | [`inspectLocalPackageResponse`](#inspectlocalpackageresponse-data-payload) |
| [`GetTimelineInfo`](#gettimelineinfo) | `google.protobuf.Empty` | [`TimeLineInformationResponse`](#timelineinformationresponse) | [`timelineData`](#timelinedata) |

---

## Methods

### `InstallPackage`

Install a package from a local path.

- **Request:** [`InstallPackageRequest`](#installpackagerequest)
- **Response:** [`InstallPackageResponse`](#installpackageresponse)
- **Response data payload:** [`installResponse`](#installresponse)

Installation always runs full signature and integrity checks (see
[Package Integrity](../architecture/package-integrity.md)), regardless of
`downgrade` or who calls the RPC. A package that fails verification is
rejected at this step and is not installed (see
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
- **Response:** [`RemovePackageResponse`](#removepackageresponse)
- **Response data payload:** *(none)*

### `StartPackage`

Start one or more packages.

- **Request:** [`StartPackageRequest`](#startpackagerequest--stoppackagerequest)
- **Response:** [`StartPackageResponse`](#startpackageresponse)
- **Response data payload:** array of [`packageOpResult`](#packageopresult)

### `StopPackage`

Stop one or more packages.

- **Request:** [`StopPackageRequest`](#startpackagerequest--stoppackagerequest)
- **Response:** [`StopPackageResponse`](#stoppackageresponse)
- **Response data payload:** array of [`packageOpResult`](#packageopresult)

### `WipePackageData`

Clear stored data for one or more packages.

- **Request:** [`WipePackageDataRequest`](#wipepackagedatarequest)
- **Response:** [`WipePackageDataResponse`](#wipepackagedataresponse)
- **Response data payload:** array of [`packageOpResult`](#packageopresult)

### `ListPackagesStatus`

List installed packages and their status.

- **Request:** `google.protobuf.Empty`
- **Response:** [`ListPackagesStatusResponse`](#listpackagesstatusresponse)
- **Response data payload:** array of [`packageStatusEntry`](#packagestatusentry)

### `GetPackageInfo`

Details for one installed package.

- **Request:** [`PackageInformationRequest`](#packageinformationrequest)
- **Response:** [`PackageInformationResponse`](#packageinformationresponse)
- **Response data payload:** [`packageInfoResponse`](#packageinforesponse)

### `GetAllPackageInfo`

Details for all installed packages.

- **Request:** `google.protobuf.Empty`
- **Response:** [`AllPackageInformationResponse`](#allpackageinformationresponse)
- **Response data payload:** array of [`packageInfoResponse`](#packageinforesponse)

### `InspectLocalPackage`

Details for one package that hasn't been installed.

- **Request:** [`InspectLocalPackageRequest`](#inspectlocalpackagerequest)
- **Response:** [`InspectLocalPackageResponse`](#inspectlocalpackageresponse)
- **Response data payload:** [`inspectLocalPackageResponse`](#inspectlocalpackageresponse-data-payload)

### `GetTimelineInfo`

Daemon timeline / uptime information.

- **Request:** `google.protobuf.Empty`
- **Response:** [`TimeLineInformationResponse`](#timelineinformationresponse)
- **Response data payload:** [`timelineData`](#timelinedata)

---

## Request messages

`ListPackagesStatus`, `GetAllPackageInfo`, and `GetTimelineInfo` take
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

### `PackageInformationRequest`

| Field | Type | Meaning |
| --- | --- | --- |
| `package_name` | `string` | Name of the installed package to query |

### `InspectLocalPackageRequest`

| Field | Type | Meaning |
| --- | --- | --- |
| `package_path` | `string` | Path to an SSAM package file to query without installing |

---

## Response messages

Every response message shares an identical shape:

| Field | Type | Meaning |
| --- | --- | --- |
| `result` | `string` | A JSON document describing the outcome |

The `result` string is **not** free-form text. It is a serialized JSON
document that conforms to
[`remocon-responses.schema.json`](../../libssam/data/remocon-responses.schema.json)
(JSON Schema, draft 2020-12): `schema_version` and `success` are always
present; on success, `data` carries the per-RPC response data payload; on
failure, `reason` carries a human-readable message instead. `data` and
`reason` are mutually exclusive.

| Field | Type | Presence | Meaning |
| --- | --- | --- | --- |
| `schema_version` | `string` | always | Response schema version. Current value is `"2"`. A missing `schema_version` is treated as the current version. |
| `success` | `bool` | always | `true` for a successful call, `false` for a failure. |
| `data` | [resp data payload](#response-data-payload) | success only | Omitted when the operation returns no data (e.g. `RemovePackage`); see [API at a Glance](#api-at-a-glance) for the exact shape per RPC. |
| `reason` | `string` | failure only | Human-readable failure message. |

Two examples, showing a success and a failure:

```json
{ "schema_version": "2", "success": true, "data": { "package_name": "navi", "status": "Running" } }
```

```json
{ "schema_version": "2", "success": false, "reason": "Package not found" }
```

Each response message below is a distinct proto message that shares this
shape; it is returned by its own method.

### `InstallPackageResponse`

- **Returned by:** [`InstallPackage`](#installpackage)
- **Response data payload:** [`installResponse`](#installresponse)

### `RemovePackageResponse`

- **Returned by:** [`RemovePackage`](#removepackage)
- **Response data payload:** *(none)*

### `StartPackageResponse`

- **Returned by:** [`StartPackage`](#startpackage)
- **Response data payload:** array of [`packageOpResult`](#packageopresult)

### `StopPackageResponse`

- **Returned by:** [`StopPackage`](#stoppackage)
- **Response data payload:** array of [`packageOpResult`](#packageopresult)

### `WipePackageDataResponse`

- **Returned by:** [`WipePackageData`](#wipepackagedata)
- **Response data payload:** array of [`packageOpResult`](#packageopresult)

### `ListPackagesStatusResponse`

- **Returned by:** [`ListPackagesStatus`](#listpackagesstatus)
- **Response data payload:** array of [`packageStatusEntry`](#packagestatusentry)

### `PackageInformationResponse`

- **Returned by:** [`GetPackageInfo`](#getpackageinfo)
- **Response data payload:** [`packageInfoResponse`](#packageinforesponse)

### `AllPackageInformationResponse`

- **Returned by:** [`GetAllPackageInfo`](#getallpackageinfo)
- **Response data payload:** array of [`packageInfoResponse`](#packageinforesponse)

### `InspectLocalPackageResponse`

- **Returned by:** [`InspectLocalPackage`](#inspectlocalpackage)
- **Response data payload:** [`inspectLocalPackageResponse`](#inspectlocalpackageresponse-data-payload)

### `TimeLineInformationResponse`

- **Returned by:** [`GetTimelineInfo`](#gettimelineinfo)
- **Response data payload:** [`timelineData`](#timelinedata)

---

## Response data payload

### Objects

#### `packageStatusEntry`

| Field | Type | Meaning |
| --- | --- | --- |
| `package_name` | `string` | Installed package name |
| `status` | `string` | Package state (`Ready`, `Running`, `Broken`, …) — corresponds to the daemon's [package states](../architecture/execution.md#package-state-machine) |

#### `packageInfo`

Essential package metadata.

| Field | Type | Meaning |
| --- | --- | --- |
| `package_name` | `string` | Package name |
| `version` | `string` | Package version |
| `description` | `string` | Package description |

#### `packageInfoResponse`

Returned by `GetPackageInfo` (one) and `GetAllPackageInfo` (array). Unifies the
healthy and broken cases.

| Field | Type | Presence | Meaning |
| --- | --- | --- | --- |
| `broken` | `bool` | always | Whether the package failed to load |
| `name` | `string` | always | Package name |
| `status` | `string` | always | Package state |
| `package_path` | `string` | always | On-device package path |
| `metadata` | [`packageInfo`](#packageinfo) \| `null` | optional | Present for a healthy package |
| `quota` | [`quotaInformation`](#quotainformation) \| `null` | optional | Data-quota limits, when known |
| `error_summary` | `string` \| `null` | optional | Short error, for a broken package |
| `error_details` | `string` \| `null` | optional | Detailed error, for a broken package |

#### `quotaInformation`

| Field | Type | Meaning |
| --- | --- | --- |
| `enabled` | `bool` | Whether a data quota is enforced |
| `limit` | `integer` | Quota limit in bytes (≥ 0) |

#### `packageOpResult`

Per-package outcome for the bulk operations (`StartPackage`, `StopPackage`,
`WipePackageData`), which each return an **array** with one entry per package.

| Field | Type | Presence | Meaning |
| --- | --- | --- | --- |
| `package_name` | `string` | always | The package this entry is about |
| `success` | `bool` | always | Whether the operation succeeded for this package |
| `message` | `string` | optional | Detail, typically on failure |

The response's own `success` and each entry's `success` are independent:
the response can report `true` while individual entries report `false`.

#### `installResponse`

| Field | Type | Meaning |
| --- | --- | --- |
| `metadata` | [`packageInfo`](#packageinfo) | Metadata of the newly installed package |

#### `inspectLocalPackageResponse` (data payload)

| Field | Type | Meaning |
| --- | --- | --- |
| `metadata` | [`packageInfo`](#packageinfo) | Metadata read from the SSAM package file |
| `package_path` | `string` | Path that was queried |

#### `timelineData`

| Field | Type | Meaning |
| --- | --- | --- |
| `events` | array of [`timelineEvent`](#timelineevent) | Recorded lifecycle events |
| `ssamd_uptime` | [`duration`](#duration) | Daemon uptime |

#### `timelineEvent`

| Field | Type | Meaning |
| --- | --- | --- |
| `pkg` | `string` | Package name |
| `phase` | `string` | Lifecycle phase |
| `duration_ns` | `integer` | Phase duration in nanoseconds (≥ 0) |
| `kind` | `string` | `started` or `completed` |

#### `duration`

Standard Rust `Duration` serialization.

| Field | Type | Meaning |
| --- | --- | --- |
| `secs` | `integer` | Whole seconds (≥ 0) |
| `nanos` | `integer` | Sub-second nanoseconds (0–999 999 999) |

---

## Examples

`GetPackageInfo` — success. A complete response, with `data` fully
populated:

```json
{
  "schema_version": "2",
  "success": true,
  "data": {
    "broken": false,
    "name": "navi",
    "status": "Running",
    "package_path": "/var/lib/ssam/packages/navi",
    "metadata": {
      "package_name": "navi",
      "version": "1.4.0",
      "description": "Navigation service"
    },
    "quota": {
      "enabled": true,
      "limit": 536870912
    },
    "error_summary": null,
    "error_details": null
  }
}
```

`ListPackagesStatus` — success:

```json
{
  "schema_version": "2",
  "success": true,
  "data": [
    { "package_name": "navi", "status": "Running" },
    { "package_name": "cluster", "status": "Ready" }
  ]
}
```

`StartPackage` — a successful call with a per-package failure:

```json
{
  "schema_version": "2",
  "success": true,
  "data": [
    { "package_name": "navi", "success": true },
    { "package_name": "cluster", "success": false, "message": "already running" }
  ]
}
```

`GetPackageInfo` — failure:

```json
{ "schema_version": "2", "success": false, "reason": "Package 'navi' not found" }
```
