# Writing a Remocon Client

<p>
  <a href="writing-a-remocon-client.md"><img src="https://img.shields.io/badge/lang-English-blue" alt="English"></a>
</p>

이 문서는 `ssamd` 가 gRPC로 제공하는 **Remocon** API를 사용하여 클라이언트를
작성하는 방법을 설명합니다. `ssam` CLI도 이 프로토콜을 활용해 작성되었습니다.
전체 프로토콜은 [Remocon gRPC 제어 API 레퍼런스](../reference/remocon-rpc.ko.md)를
참고하세요.

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

이 RPC로 다양한 형태의 client를 만들 수 있습니다. 예를 들면:

- **컨테이너 lifecycle을 직접 관리하는 container manager** — 자체 정책에
  따라 언제 설치·시작·중지·제거할지 스스로 판단.
- **롤아웃을 전담하는 update manager** — 새 버전을 언제 설치할지,
  downgrade나 rollback 여부를 자체 로직으로 판단.

---

## Get the protocol

서비스는
[`libssam/data/ssam.remocon.proto`](../../libssam/data/ssam.remocon.proto)
(proto 패키지 `ssamd.remocon`)에 정의되어 있습니다. 사용하는 언어의
gRPC/protoc 도구로 스텁을 생성하세요 — 코드 생성 자체는 SSAM 고유의 절차가
아닙니다. 예를 들어 이 저장소의 `libssam/build.rs` 는
`tonic_prost_build::compile_protos` 를 호출해 Rust 스텁을 생성하며, 다른
언어에도 각자의 대응 도구가 있습니다(Python은 `grpc_tools.protoc`, Go는
`protoc-gen-go` 등).

```rust
// build.rs
fn main() -> anyhow::Result<()> {
    tonic_prost_build::compile_protos("data/ssam.remocon.proto")?;
    Ok(())
}
```

---

## Connect to `ssamd`

`ssamd` 는 고정된 Linux abstract Unix domain socket `@ssamd`에서 로컬 gRPC
연결을 받습니다. 원격 TCP 연결은 지원하지 않습니다. peer의 effective GID가 데몬의
effective GID와 일치하거나 `[rpc].allowed_gids` 항목에 포함되어야 하며, 데몬은
`SO_PEERCRED`로 이를 검사합니다. 인증에는 UID를 사용하지 않으며, 추가 목록의
기본값은 비어 있습니다([데몬 설정 레퍼런스](../reference/daemon-config.ko.md#rpc) 참조).

예를 들어 Rust로는:

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

모든 Remocon request에는 `x-ssam-api-version` metadata가 있어야 합니다. 이 값은
proto 파일의 `ApiVersion` enum에 정의된 `CURRENT` 값과 일치해야 합니다. 숫자
값을 직접 하드코딩하지 말고 codegen으로 생성된 해당 enum 값을 사용하는 것을
권장합니다.

`x-ssam-api-version` metadata가 없거나 잘못된 request는 RPC handler가 실행되기
전에 gRPC `FAILED_PRECONDITION`으로 거부됩니다.

단, 개행·NUL byte가 포함된 header처럼 client의 metadata 생성 단계 또는 HTTP/2
transport에서 header 자체가 먼저 거부되는 경우에는 `FAILED_PRECONDITION` status가
보장되지 않습니다.

## Call a method

레퍼런스의
[API at a Glance](../reference/remocon-rpc.ko.md#api-at-a-glance)에서
method를 고르고 그
[request message](../reference/remocon-rpc.ko.md#request-messages)를
채우세요. 예를 들어
[`ListPackageInfo`](../reference/remocon-rpc.ko.md#listpackageinfo)는
[`ListPackageInfoRequest`](../reference/remocon-rpc.ko.md#listpackageinforequest)를
받습니다. 아래 `generated::remocon`은 `ssamd.remocon` proto로 생성한 stub 모듈이며
생성한 module layout이 다르면 해당 경로로 바꾸세요:

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

- 각 method는 선언된 protobuf response message를 payload로 담은 gRPC
  response를 반환합니다.
- RPC 실패는 gRPC `Status`로 반환됩니다. 성공한 response field는 선언된
  protobuf message에서 직접 읽고, response별 정확한 field는 레퍼런스 문서를
  참고하세요.

---

## Handle bulk per-package results

다음 세 method는 패키지에 대한 요청을 한 번에 받을 수 있으며, 결과도
요청된 패키지별로 배열 형태로 반환합니다:

- [`StartPackage`](../reference/remocon-rpc.ko.md#startpackage)
- [`StopPackage`](../reference/remocon-rpc.ko.md#stoppackage)
- [`WipePackageData`](../reference/remocon-rpc.ko.md#wipepackagedata)

그래서 다음과 같은 사항에 주의가 필요합니다.
- `package_names` 목록이 비어 있으면(`[]`) 설치된 **모든** 패키지를 대상으로 합니다(
  [Request messages](../reference/remocon-rpc.ko.md#request-messages) 참조).
  빈 목록이 실수인지 의도한 것인지 method 자체는 판단할 수 없어 별도의 방어
  코드가 없으니, 빈 목록을 그대로 전달하기 전에 확인 절차나 안전장치를
  두는 걸 권장합니다.
- 중복된 패키지 이름은 한 번만 처리됩니다.
- 반환되는 [`PackageActionResult`](../reference/remocon-rpc.ko.md#packageactionresult)
  배열은 패키지당 한 항목씩 담습니다. RPC가 성공해도 개별 작업은 실패할
  수 있으므로 각 항목을 따로 확인해야 합니다.

예를 들어:

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

이 RPC는 로컬 abstract Unix domain socket을 사용하며 원격 TCP 연결을 지원하지
않습니다. 데몬은 RPC를 dispatch하기 전에 `SO_PEERCRED`를 검사하고 peer의
effective GID가 데몬의 effective GID와 일치하거나 `[rpc].allowed_gids` 항목에
포함되는 경우 허용합니다. 인증에는 UID를 사용하지 않습니다.

- 패키지 진위 검증은 별도의 보안 계층으로 적용됩니다: 허용된 설치 RPC는
항상 검증을 거치므로, 서명되지 않거나 변조된 패키지는 설치 단계에서 거부되어
로드·실행되지 않습니다(
[제어 인터페이스 보안](../architecture/package-integrity.ko.md#control-interface-security-rpc) 참조).
