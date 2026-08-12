# Remocon gRPC 제어 API 레퍼런스

<p>
  <a href="remocon-rpc.md"><img src="https://img.shields.io/badge/lang-English-blue" alt="English"></a>
</p>

이 문서는 패키지를 제어하기 위해 `ssamd`가 gRPC로 제공하는 Remocon API를
설명합니다.

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

| 속성 | 값 |
| --- | --- |
| 전송 | gRPC (HTTP/2), proto3 |
| gRPC package | `ssamd.remocon` (proto3) |
| 호스트 | [`rpc_bind_ip`](daemon-config.ko.md#rpc_bind_ip) — 기본 `127.0.0.1` |
| 포트 | **63737** (고정 SSAM 제어 포트) |
| 인증 | 전송/RPC 레벨에서는 없음 |

---

## API at a Glance

| Method | Request | Response | Response data payload |
| --- | --- | --- | --- |
| [`InstallPackage`](#installpackage) | [`InstallPackageRequest`](#installpackagerequest) | [`InstallPackageResponse`](#installpackageresponse) | [`installResponse`](#installresponse) |
| [`RemovePackage`](#removepackage) | [`RemovePackageRequest`](#removepackagerequest) | [`RemovePackageResponse`](#removepackageresponse) | *(없음)* |
| [`StartPackage`](#startpackage) | [`StartPackageRequest`](#startpackagerequest--stoppackagerequest) | [`StartPackageResponse`](#startpackageresponse) | [`packageOpResult`](#packageopresult) 배열 |
| [`StopPackage`](#stoppackage) | [`StopPackageRequest`](#startpackagerequest--stoppackagerequest) | [`StopPackageResponse`](#stoppackageresponse) | [`packageOpResult`](#packageopresult) 배열 |
| [`WipePackageData`](#wipepackagedata) | [`WipePackageDataRequest`](#wipepackagedatarequest) | [`WipePackageDataResponse`](#wipepackagedataresponse) | [`packageOpResult`](#packageopresult) 배열 |
| [`ListPackagesStatus`](#listpackagesstatus) | `google.protobuf.Empty` | [`ListPackagesStatusResponse`](#listpackagesstatusresponse) | [`packageStatusEntry`](#packagestatusentry) 배열 |
| [`GetPackageInfo`](#getpackageinfo) | [`PackageInformationRequest`](#packageinformationrequest) | [`PackageInformationResponse`](#packageinformationresponse) | [`packageInfoResponse`](#packageinforesponse) |
| [`GetAllPackageInfo`](#getallpackageinfo) | `google.protobuf.Empty` | [`AllPackageInformationResponse`](#allpackageinformationresponse) | [`packageInfoResponse`](#packageinforesponse) 배열 |
| [`InspectLocalPackage`](#inspectlocalpackage) | [`InspectLocalPackageRequest`](#inspectlocalpackagerequest) | [`InspectLocalPackageResponse`](#inspectlocalpackageresponse) | [`inspectLocalPackageResponse`](#inspectlocalpackageresponse-data-payload) |
| [`GetTimelineInfo`](#gettimelineinfo) | `google.protobuf.Empty` | [`TimeLineInformationResponse`](#timelineinformationresponse) | [`timelineData`](#timelinedata) |

---

## Methods

### `InstallPackage`

로컬 경로에서 패키지 설치.

- **Request:** [`InstallPackageRequest`](#installpackagerequest)
- **Response:** [`InstallPackageResponse`](#installpackageresponse)
- **Response data payload:** [`installResponse`](#installresponse)

설치는 `downgrade` 여부나 호출 주체와 무관하게 항상 서명·무결성 검사를 거칩니다
([패키지 무결성](../architecture/package-integrity.ko.md) 참조). 서명 검증에 실패한
패키지는 이 단계에서 거부되어 설치되지 않습니다([제어 인터페이스 보안]
(../architecture/package-integrity.ko.md#control-interface-security-rpc) 참조).

같은 패키지의 현재 설치된 버전과 비교한 결과에 따라 결정됩니다:

- **동일 버전** — `downgrade` 값과 무관하게 항상 거부됩니다.
- **낮은 버전** — `downgrade: true`를 지정하지 않으면 거부됩니다.
- **높은 버전** — `downgrade: true`를 지정하면 오히려 거부됩니다. 업그레이드에는
  기본값(`false`)을 사용하세요.

### `RemovePackage`

설치된 패키지 제거.

- **Request:** [`RemovePackageRequest`](#removepackagerequest)
- **Response:** [`RemovePackageResponse`](#removepackageresponse)
- **Response data payload:** *(없음)*

### `StartPackage`

하나 이상의 패키지 시작.

- **Request:** [`StartPackageRequest`](#startpackagerequest--stoppackagerequest)
- **Response:** [`StartPackageResponse`](#startpackageresponse)
- **Response data payload:** [`packageOpResult`](#packageopresult) 배열

### `StopPackage`

하나 이상의 패키지 중지.

- **Request:** [`StopPackageRequest`](#startpackagerequest--stoppackagerequest)
- **Response:** [`StopPackageResponse`](#stoppackageresponse)
- **Response data payload:** [`packageOpResult`](#packageopresult) 배열

### `WipePackageData`

하나 이상의 패키지의 저장 데이터 삭제.

- **Request:** [`WipePackageDataRequest`](#wipepackagedatarequest)
- **Response:** [`WipePackageDataResponse`](#wipepackagedataresponse)
- **Response data payload:** [`packageOpResult`](#packageopresult) 배열

### `ListPackagesStatus`

설치된 패키지와 상태 목록.

- **Request:** `google.protobuf.Empty`
- **Response:** [`ListPackagesStatusResponse`](#listpackagesstatusresponse)
- **Response data payload:** [`packageStatusEntry`](#packagestatusentry) 배열

### `GetPackageInfo`

설치된 패키지의 상세 정보.

- **Request:** [`PackageInformationRequest`](#packageinformationrequest)
- **Response:** [`PackageInformationResponse`](#packageinformationresponse)
- **Response data payload:** [`packageInfoResponse`](#packageinforesponse)

### `GetAllPackageInfo`

설치된 모든 패키지의 상세 정보.

- **Request:** `google.protobuf.Empty`
- **Response:** [`AllPackageInformationResponse`](#allpackageinformationresponse)
- **Response data payload:** [`packageInfoResponse`](#packageinforesponse) 배열

### `InspectLocalPackage`

설치되지 않은 패키지의 상세 정보.

- **Request:** [`InspectLocalPackageRequest`](#inspectlocalpackagerequest)
- **Response:** [`InspectLocalPackageResponse`](#inspectlocalpackageresponse)
- **Response data payload:** [`inspectLocalPackageResponse`](#inspectlocalpackageresponse-data-payload)

### `GetTimelineInfo`

데몬 타임라인 / 가동 시간 정보.

- **Request:** `google.protobuf.Empty`
- **Response:** [`TimeLineInformationResponse`](#timelineinformationresponse)
- **Response data payload:** [`timelineData`](#timelinedata)

---

## Request messages

`ListPackagesStatus`, `GetAllPackageInfo`, `GetTimelineInfo`는
`google.protobuf.Empty`를 받으며 자체 요청 필드가 없습니다 — 아래
메시지들만 필드를 가집니다.

### `InstallPackageRequest`

| 필드 | 타입 | 의미 |
| --- | --- | --- |
| `package_path` | `string` | 설치할 SSAM 패키지 파일 경로 |
| `downgrade` | `bool` | 설치된 것보다 낮은 버전 설치 |
| `remove_data` | `bool` | 설치 시 패키지의 기존 데이터도 제거 |

### `RemovePackageRequest`

| 필드 | 타입 | 의미 |
| --- | --- | --- |
| `package_name` | `string` | 제거할 설치된 패키지 이름 |

### `StartPackageRequest` / `StopPackageRequest`

| 필드 | 타입 | 의미 |
| --- | --- | --- |
| `package_names` | `repeated string` | 시작/중지할 한 개 이상의 패키지. 목록이 비어 있으면(`[]`) 설치된 모든 패키지를 대상으로 합니다. |

### `WipePackageDataRequest`

| 필드 | 타입 | 의미 |
| --- | --- | --- |
| `package_names` | `repeated string` | 데이터를 삭제할 한 개 이상의 패키지. 목록이 비어 있으면(`[]`) 설치된 모든 패키지를 대상으로 합니다. |

### `PackageInformationRequest`

| 필드 | 타입 | 의미 |
| --- | --- | --- |
| `package_name` | `string` | 조회할 설치된 패키지 이름 |

### `InspectLocalPackageRequest`

| 필드 | 타입 | 의미 |
| --- | --- | --- |
| `package_path` | `string` | 설치 없이 조회할 SSAM 패키지 파일 경로 |

---

## Response messages

모든 응답 메시지는 동일한 구조를 공유합니다:

| 필드 | 타입 | 의미 |
| --- | --- | --- |
| `result` | `string` | 결과를 기술하는 JSON 문서 |

`result` string은 자유 형식 텍스트가 **아닙니다**. 고정된 스키마 —
[`remocon-responses.schema.json`](../../libssam/data/remocon-responses.schema.json)
(JSON Schema, draft 2020-12) — 을 따르는 직렬화된 JSON 문서입니다:
`schema_version`과 `success`는 항상 존재하고, 성공 시 `data`가 RPC별
response data payload를 담으며, 실패 시엔 대신 `reason`이 사람이 읽을 수
있는 메시지를 담습니다. `data`와 `reason`은 상호 배타적입니다.

| 필드 | 타입 | 존재 | 의미 |
| --- | --- | --- | --- |
| `schema_version` | `string` | 항상 | 응답 스키마 버전. 현재 값은 `"2"`. `schema_version`이 없으면 현재 버전으로 간주합니다. |
| `success` | `bool` | 항상 | 성공이면 `true`, 실패면 `false`. |
| `data` | [resp data payload](#response-data-payload) | 성공 시 | 반환할 데이터가 없으면 생략(예: `RemovePackage`); RPC별 정확한 형태는 [API at a Glance](#api-at-a-glance) 참조. |
| `reason` | `string` | 실패 시 | 사람이 읽을 수 있는 실패 메시지. |

성공과 실패 각각의 예:

```json
{ "schema_version": "2", "success": true, "data": { "package_name": "navi", "status": "Running" } }
```

```json
{ "schema_version": "2", "success": false, "reason": "Package not found" }
```

각 응답 메시지는 이 구조를 공유하는 별개의 proto 메시지이며, 자신만의
메서드가 반환합니다.

### `InstallPackageResponse`

- **Returned by:** [`InstallPackage`](#installpackage)
- **Response data payload:** [`installResponse`](#installresponse)

### `RemovePackageResponse`

- **Returned by:** [`RemovePackage`](#removepackage)
- **Response data payload:** *(없음)*

### `StartPackageResponse`

- **Returned by:** [`StartPackage`](#startpackage)
- **Response data payload:** [`packageOpResult`](#packageopresult) 배열

### `StopPackageResponse`

- **Returned by:** [`StopPackage`](#stoppackage)
- **Response data payload:** [`packageOpResult`](#packageopresult) 배열

### `WipePackageDataResponse`

- **Returned by:** [`WipePackageData`](#wipepackagedata)
- **Response data payload:** [`packageOpResult`](#packageopresult) 배열

### `ListPackagesStatusResponse`

- **Returned by:** [`ListPackagesStatus`](#listpackagesstatus)
- **Response data payload:** [`packageStatusEntry`](#packagestatusentry) 배열

### `PackageInformationResponse`

- **Returned by:** [`GetPackageInfo`](#getpackageinfo)
- **Response data payload:** [`packageInfoResponse`](#packageinforesponse)

### `AllPackageInformationResponse`

- **Returned by:** [`GetAllPackageInfo`](#getallpackageinfo)
- **Response data payload:** [`packageInfoResponse`](#packageinforesponse) 배열

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

| 필드 | 타입 | 의미 |
| --- | --- | --- |
| `package_name` | `string` | 설치된 패키지 이름 |
| `status` | `string` | 패키지 상태(`Ready`, `Running`, `Broken`, …) — 데몬의 [패키지 상태](../architecture/execution.ko.md#package-state-machine)에 대응 |

#### `packageInfo`

핵심 패키지 메타데이터.

| 필드 | 타입 | 의미 |
| --- | --- | --- |
| `package_name` | `string` | 패키지 이름 |
| `version` | `string` | 패키지 버전 |
| `description` | `string` | 패키지 설명 |

#### `packageInfoResponse`

`GetPackageInfo`(하나)와 `GetAllPackageInfo`(배열)가 반환. 정상과 손상(broken)
경우를 하나로 통합합니다.

| 필드 | 타입 | 존재 | 의미 |
| --- | --- | --- | --- |
| `broken` | `bool` | 항상 | 패키지 로드 실패 여부 |
| `name` | `string` | 항상 | 패키지 이름 |
| `status` | `string` | 항상 | 패키지 상태 |
| `package_path` | `string` | 항상 | 디바이스상의 패키지 경로 |
| `metadata` | [`packageInfo`](#packageinfo) \| `null` | 선택 | 정상 패키지일 때 존재 |
| `quota` | [`quotaInformation`](#quotainformation) \| `null` | 선택 | 알려진 경우 데이터 쿼터 한도 |
| `error_summary` | `string` \| `null` | 선택 | 손상 패키지의 짧은 오류 |
| `error_details` | `string` \| `null` | 선택 | 손상 패키지의 상세 오류 |

#### `quotaInformation`

| 필드 | 타입 | 의미 |
| --- | --- | --- |
| `enabled` | `bool` | 데이터 쿼터 강제 여부 |
| `limit` | `integer` | 쿼터 한도(바이트, ≥ 0) |

#### `packageOpResult`

일괄 작업(`StartPackage`, `StopPackage`, `WipePackageData`)의 패키지별 결과. 이
작업들은 패키지당 한 항목씩 담은 **배열**을 반환합니다.

| 필드 | 타입 | 존재 | 의미 |
| --- | --- | --- | --- |
| `package_name` | `string` | 항상 | 이 항목이 가리키는 패키지 |
| `success` | `bool` | 항상 | 해당 패키지에 대한 작업 성공 여부 |
| `message` | `string` | 선택 | 상세, 보통 실패 시 |

응답 자체의 `success`와 개별 항목의 `success`는 서로 독립적입니다 —
응답은 `true`를 보고하면서도 개별 항목은 `false`를 보고할 수 있습니다.

#### `installResponse`

| 필드 | 타입 | 의미 |
| --- | --- | --- |
| `metadata` | [`packageInfo`](#packageinfo) | 새로 설치된 패키지의 메타데이터 |

#### `inspectLocalPackageResponse` (data payload)

| 필드 | 타입 | 의미 |
| --- | --- | --- |
| `metadata` | [`packageInfo`](#packageinfo) | SSAM 패키지 파일에서 읽은 메타데이터 |
| `package_path` | `string` | 조회한 경로 |

#### `timelineData`

| 필드 | 타입 | 의미 |
| --- | --- | --- |
| `events` | [`timelineEvent`](#timelineevent) 배열 | 기록된 수명주기 이벤트 |
| `ssamd_uptime` | [`duration`](#duration) | 데몬 가동 시간 |

#### `timelineEvent`

| 필드 | 타입 | 의미 |
| --- | --- | --- |
| `pkg` | `string` | 패키지 이름 |
| `phase` | `string` | 수명주기 단계 |
| `duration_ns` | `integer` | 단계 소요 시간(나노초, ≥ 0) |
| `kind` | `string` | `started` 또는 `completed` |

#### `duration`

표준 Rust `Duration` 직렬화.

| 필드 | 타입 | 의미 |
| --- | --- | --- |
| `secs` | `integer` | 초(≥ 0) |
| `nanos` | `integer` | 초 미만 나노초(0–999 999 999) |

---

## Examples

`GetPackageInfo` — 성공. `data`까지 완전히 채워진 전체 응답:

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

`ListPackagesStatus` — 성공:

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

`StartPackage` — 호출 자체는 성공이나 일부 패키지가 실패:

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

`GetPackageInfo` — 실패:

```json
{ "schema_version": "2", "success": false, "reason": "Package 'navi' not found" }
```
