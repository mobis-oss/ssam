# Remocon gRPC 제어 API 레퍼런스

<p>
  <a href="remocon-rpc.md"><img src="https://img.shields.io/badge/lang-English-blue" alt="English"></a>
</p>

이 문서는 패키지를 제어하기 위해 `ssamd` 가 *gRPC* 로 제공하는 Remocon API를 설명합니다.

> gRPC 클라이언트 작성의 단계별 안내는 **[Remocon 클라이언트 작성](../guides/writing-a-remocon-client.ko.md)** 을
> 참고하세요. 이 문서는 API **레퍼런스** 입니다.

---

## Table of Contents

1. [Connection](#connection)
2. [API at a Glance](#api-at-a-glance)
3. [Methods](#methods)
4. [Request messages](#request-messages)
5. [Response messages](#response-messages)
6. [Response data types](#response-data-types)
7. [Examples](#examples)

---

## Connection

| 속성 | 값 |
| --- | --- |
| 전송 | gRPC (HTTP/2), proto3 |
| gRPC package | `ssamd.remocon` (proto3) |
| 엔드포인트 | Linux abstract Unix domain socket `@ssamd` |
| 인증 | `SO_PEERCRED`: peer의 effective GID가 데몬의 effective GID 또는 `[rpc].allowed_gids` 항목과 일치해야 함 |

`ssamd` 는 RPC를 dispatch하기 전에 `SO_PEERCRED`에서 peer의 effective GID를 읽습니다.
이 GID가 데몬의 effective GID와 같거나 `[rpc].allowed_gids`에 지정된 추가 GID 중
하나와 같으면 허용합니다. 목록의 기본값은 빈 목록이며, 인증에는 UID를 사용하지
않습니다.

---

### API version metadata

모든 Remocon RPC request에는 `x-ssam-api-version` metadata가 있어야 합니다.
이 metadata의 값은 proto 파일의 `ApiVersion` enum에 정의된 `CURRENT` 값과
일치해야 하며, `ApiVersion.UNSPECIFIED`는 허용되는 request version이 아닙니다.
숫자 값을 직접 하드코딩하지 말고 codegen으로 생성된 해당 enum 값을 사용하는
것을 권장합니다.

`x-ssam-api-version` metadata가 없거나 잘못된 request는 RPC handler가 실행되기
전에 gRPC `FAILED_PRECONDITION`으로 거부됩니다.

단, 개행이나 NUL byte가 포함된 header처럼 client의 metadata 생성 단계 또는 HTTP/2
transport에서 header 자체가 먼저 거부되는 경우에는 `FAILED_PRECONDITION` status가
보장되지 않습니다.

## API at a Glance

| Method | Request | Response | Response data type |
| --- | --- | --- | --- |
| [`InstallPackage`](#installpackage) | [`InstallPackageRequest`](#installpackagerequest) | [`InstallPackageResponse`](#installpackageresponse) | [`PackageData`](#packagedata) |
| [`RemovePackage`](#removepackage) | [`RemovePackageRequest`](#removepackagerequest) | `google.protobuf.Empty` | *(없음)* |
| [`StartPackage`](#startpackage) | [`StartPackageRequest`](#startpackagerequest--stoppackagerequest) | [`StartPackageResponse`](#startpackageresponse) | array of [`PackageActionResult`](#packageactionresult) |
| [`StopPackage`](#stoppackage) | [`StopPackageRequest`](#startpackagerequest--stoppackagerequest) | [`StopPackageResponse`](#stoppackageresponse) | array of [`PackageActionResult`](#packageactionresult) |
| [`WipePackageData`](#wipepackagedata) | [`WipePackageDataRequest`](#wipepackagedatarequest) | [`WipePackageDataResponse`](#wipepackagedataresponse) | array of [`PackageActionResult`](#packageactionresult) |
| [`ListPackages`](#listpackages) | `google.protobuf.Empty` | [`ListPackagesResponse`](#listpackagesresponse) | array of [`PackageStatusEntry`](#packagestatusentry) |
| [`ListPackageInfo`](#listpackageinfo) | [`ListPackageInfoRequest`](#listpackageinforequest) | [`ListPackageInfoResponse`](#listpackageinforesponse) | array of [`PackageInfo`](#packageinfo) |
| [`ListPackageInfoFromFiles`](#listpackageinfofromfiles) | [`ListPackageInfoFromFilesRequest`](#listpackageinfofromfilesrequest) | [`ListPackageInfoFromFilesResponse`](#listpackageinfofromfilesresponse) | array of [`InspectLocalPackageResult`](#inspectlocalpackageresult) |
| [`GetTimelineInfo`](#gettimelineinfo) | `google.protobuf.Empty` | [`TimelineData`](#timelinedata) | [`TimelineData`](#timelinedata) |

---

## Methods

### `InstallPackage`

로컬 경로에서 패키지 설치.

- **Request:** [`InstallPackageRequest`](#installpackagerequest)
- **Response:** [`InstallPackageResponse`](#installpackageresponse)
- **Response data type:** [`PackageData`](#packagedata)

허용된 설치 RPC는 `downgrade` 설정과 무관하게 항상 서명·무결성 검사를 거칩니다
([패키지 무결성](../architecture/package-integrity.ko.md) 참조). 검증에 실패한 패키지는
이 단계에서 거부되어 설치되지 않습니다([제어 인터페이스 보안]
(../architecture/package-integrity.ko.md#control-interface-security-rpc) 참조).

설치 허용 여부는 같은 패키지의 이미 설치된 버전과 비교해 결정됩니다:

- **동일 버전** — `downgrade` 값과 무관하게 항상 거부됩니다.
- **낮은 버전** — `downgrade: true` 를 지정하지 않으면 거부됩니다.
- **높은 버전** — `downgrade: true` 를 지정하면 오히려 거부됩니다. 업그레이드에는
  기본값(`false`)을 사용하세요.

### `RemovePackage`

설치된 패키지 제거.

- **Request:** [`RemovePackageRequest`](#removepackagerequest)
- **Response:** `google.protobuf.Empty`
- **Response data type:** *(없음)*

패키지가 없으면 gRPC `NotFound`를 반환하며, 그 외 제거 오류는 `Internal`을
반환합니다.

### `StartPackage`

하나 이상의 패키지 시작.

- **Request:** [`StartPackageRequest`](#startpackagerequest--stoppackagerequest)
- **Response:** [`StartPackageResponse`](#startpackageresponse)
- **Response data type:** array of [`PackageActionResult`](#packageactionresult)

### `StopPackage`

하나 이상의 패키지 중지.

- **Request:** [`StopPackageRequest`](#startpackagerequest--stoppackagerequest)
- **Response:** [`StopPackageResponse`](#stoppackageresponse)
- **Response data type:** array of [`PackageActionResult`](#packageactionresult)

### `WipePackageData`

하나 이상의 패키지의 저장 데이터 삭제.

- **Request:** [`WipePackageDataRequest`](#wipepackagedatarequest)
- **Response:** [`WipePackageDataResponse`](#wipepackagedataresponse)
- **Response data type:** array of [`PackageActionResult`](#packageactionresult)

### `ListPackages`

설치된 패키지와 상태 목록. `Broken` 항목도 포함합니다.

- **Request:** `google.protobuf.Empty`
- **Response:** [`ListPackagesResponse`](#listpackagesresponse)
- **Response data type:** array of [`PackageStatusEntry`](#packagestatusentry)

### `ListPackageInfo`

설치된 하나 이상의 패키지 상세 정보. `package_names`가 비어 있으면 설치된 모든
패키지를 반환하며, 공백만 있는 이름은 무시합니다.

- **Request:** [`ListPackageInfoRequest`](#listpackageinforequest)
- **Response:** [`ListPackageInfoResponse`](#listpackageinforesponse)
- **Response data type:** array of [`PackageInfo`](#packageinfo)

### `ListPackageInfoFromFiles`

설치하지 않고 SSAM 패키지 파일을 조회합니다. 경로 목록이 비어 있으면 빈 결과를
반환합니다. 요청한 경로 순서대로 각 파일에 대한 결과를 반환하며, 상대 경로나
검사 실패는 해당 결과의 `success=false`로 표시합니다.

- **Request:** [`ListPackageInfoFromFilesRequest`](#listpackageinfofromfilesrequest)
- **Response:** [`ListPackageInfoFromFilesResponse`](#listpackageinfofromfilesresponse)
- **Response data type:** array of [`InspectLocalPackageResult`](#inspectlocalpackageresult)

### `GetTimelineInfo`

데몬 타임라인 / 가동 시간 정보.

- **Request:** `google.protobuf.Empty`
- **Response:** [`TimelineData`](#timelinedata)
- **Response data type:** [`TimelineData`](#timelinedata)

---

## Request messages

`ListPackages`, `GetTimelineInfo` 는
`google.protobuf.Empty` 를 받으며 자체 요청 필드가 없습니다 — 아래
메시지에만 필드가 있습니다.

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

### `ListPackageInfoRequest`

| 필드 | 타입 | 의미 |
| --- | --- | --- |
| `package_names` | `repeated string` | 조회할 설치된 패키지 이름. 비어 있으면 설치된 모든 패키지 |

### `ListPackageInfoFromFilesRequest`

| 필드 | 타입 | 의미 |
| --- | --- | --- |
| `package_paths` | `repeated string` | 설치 없이 조회할 SSAM 패키지 파일 절대 경로. 빈 목록이면 빈 결과를 반환합니다 |

---

## Response messages

각 RPC의 response payload는 선언된 protobuf message입니다.

각 응답 message는 선언된 protobuf 타입이며, 다음 필드를 반환합니다.

| RPC | Response | Fields |
| --- | --- | --- |
| `InstallPackage` | `InstallPackageResponse` | `package_data` |
| `RemovePackage` | `google.protobuf.Empty` | *(없음)* |
| `StartPackage` | `StartPackageResponse` | `results` |
| `StopPackage` | `StopPackageResponse` | `results` |
| `WipePackageData` | `WipePackageDataResponse` | `results` |
| `ListPackages` | `ListPackagesResponse` | `entries` |
| `ListPackageInfo` | `ListPackageInfoResponse` | `packages`: `repeated PackageInfo` |
| `ListPackageInfoFromFiles` | `ListPackageInfoFromFilesResponse` | `packages`: `repeated InspectLocalPackageResult` |
| `GetTimelineInfo` | `TimelineData` | `events`, `ssamd_uptime` |

---

## Response data types

응답에는 다음 protobuf message가 사용됩니다.

### `PackageStatusEntry`

| 필드 | 타입 | 의미 |
| --- | --- | --- |
| `package_name` | `string` | 설치된 패키지 이름 |
| `status` | `string` | 패키지 상태(`Ready`, `Running`, `Broken`, …) — 데몬의 [패키지 상태](../architecture/execution.ko.md#package-state-machine)에 대응 |

### `PackageData`

응답에서 반환하는 패키지 데이터입니다.

| 필드 | 타입 | 의미 |
| --- | --- | --- |
| `package_name` | `string` | 패키지 이름 |
| `version` | `string` | 패키지 버전 |
| `description` | `string` | 패키지 설명 |

### `PackageInfo`

`ListPackageInfo`가 요청한 각 패키지에 대해 반환합니다. `status`가 패키지의
단일 상태값이며, 패키지가 없거나 조회 오류이면 `success=false`입니다. `Broken`
항목은 `success=true`와 오류 field를 함께 반환합니다.

| 필드 | 타입 | 존재 | 의미 |
| --- | --- | --- | --- |
| `success` | `bool` | 항상 | 패키지 조회 성공 여부 |
| `name` | `string` | 항상 | 패키지 이름 |
| `status` | `string` | 항상 | 패키지 상태 |
| `package_path` | `string` | 항상 | 디바이스상의 패키지 경로. 사용할 수 없으면 빈 값 |
| `package_data` | `PackageData` | 선택 | 정상 패키지의 패키지 데이터 |
| `quota` | `QuotaInformation` | 선택 | 정상 패키지의 데이터 쿼터 한도 |
| `error_summary` | `string` | 선택 | 표시용 조회 또는 package-load 오류 |
| `error_details` | `string` | 선택 | Broken 항목과 조회 오류의 자세한 오류 정보 |

### `QuotaInformation`

| 필드 | 타입 | 의미 |
| --- | --- | --- |
| `enabled` | `bool` | 데이터 쿼터 강제 여부 |
| `limit` | `uint64` | 쿼터 한도(바이트) |

### `PackageActionResult`

일괄 작업(`StartPackage`, `StopPackage`, `WipePackageData`)의 패키지별 결과. 이
작업들은 패키지당 한 항목씩 담은 **배열** 을 반환합니다.

| 필드 | 타입 | 존재 | 의미 |
| --- | --- | --- | --- |
| `package_name` | `string` | 항상 | 이 항목이 가리키는 패키지 |
| `success` | `bool` | 항상 | 해당 패키지에 대한 작업 성공 여부 |
| `message` | `string` | 선택 | 상세, 보통 실패 시 |

### `InstallPackageResponse`

| 필드 | 타입 | 의미 |
| --- | --- | --- |
| `package_data` | `PackageData` | 새로 설치된 패키지의 패키지 데이터 |

### `InspectLocalPackageResult`

| 필드 | 타입 | 의미 |
| --- | --- | --- |
| `package_data` | `PackageData` | SSAM 패키지 파일에서 읽은 패키지 데이터 |
| `package_path` | `string` | 조회한 경로 |
| `success` | `bool` | 패키지 검사 성공 여부 |
| `error_summary` | `string` | 표시용 검사 오류 |
| `error_details` | `string` | 선택적 자세한 오류 정보 |

### `TimelineData`

| 필드 | 타입 | 의미 |
| --- | --- | --- |
| `events` | `repeated TimelineEvent` | 기록된 수명주기 이벤트 |
| `ssamd_uptime` | `google.protobuf.Duration` | `ssamd` 시작 시점의 시스템 가동 시간 |

### `TimelineEvent`

| 필드 | 타입 | 의미 |
| --- | --- | --- |
| `pkg` | `string` | 패키지 이름 |
| `phase` | `string` | 수명주기 단계 |
| `duration_ns` | `uint64` | 단계 소요 시간(나노초) |
| `kind` | `TimelineEventKind` | 이벤트 종류 |

### `TimelineEventKind`

| 값 | 번호 | 의미 |
| --- | --- | --- |
| `TIMELINE_EVENT_KIND_UNSPECIFIED` | `0` | 종류가 지정되지 않음 |
| `STARTED` | `1` | 단계 시작 |
| `COMPLETED` | `2` | 단계 완료 |

### `google.protobuf.Duration`

| 필드 | 타입 | 의미 |
| --- | --- | --- |
| `seconds` | `int64` | 초 |
| `nanos` | `int32` | 초 미만 나노초 |

---

## Examples

다음은 실제 wire 형식이 아닌 protobuf text-format pseudo-output입니다.

`ListPackageInfo` — 성공:

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

`StartPackage`에서 일부 패키지가 실패한 경우:

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

`GetTimelineInfo` — 성공:

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

tonic을 사용하는 Rust에서는 typed response와 optional message field를 처리합니다:

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
