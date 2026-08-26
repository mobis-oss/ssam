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
4. [Call a method](#call-a-method)
5. [Parse the response](#parse-the-response)
6. [Handle bulk per-package results](#handle-bulk-per-package-results)
7. [Security considerations](#security-considerations)

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

- **엔드포인트:** `http://<host>:<port>` —
  [평문 HTTP/2, TLS 없음](../reference/remocon-rpc.ko.md#connection).
- **`<host>`:** 데몬의
  [`rpc_bind_ip`](../reference/daemon-config.ko.md#rpc_bind_ip)(기본값
  `127.0.0.1`).
- **`<port>`:** 고정된 **63737**.
- **TLS:** 없음 — 대부분의 gRPC 클라이언트 라이브러리에서 `insecure` 또는
  `plaintext` 채널 옵션을 명시해야 합니다(예: Java의 `usePlaintext()`,
  Python의 `grpc.insecure_channel`). Rust의 `tonic` 은 `http://` 스킴만으로
  평문임을 인식하므로 별도 플래그가 필요 없습니다 — 사용하는 라이브러리에서
  해당하는 옵션을 찾으세요.

예를 들어 Rust로는:

```rust
let channel = Channel::from_shared(format!("http://{ip}:{CONTROL_PORT}"))?
    .connect_timeout(GRPC_CONNECT_TIMEOUT)
    .connect()
    .await?;

let mut client = RemoconClient::new(channel);
```

---

## Call a method

레퍼런스의
[API at a Glance](../reference/remocon-rpc.ko.md#api-at-a-glance)에서
method를 고르고 그
[request message](../reference/remocon-rpc.ko.md#request-messages)를
채우세요. 예를 들어
[`ListPackageInfo`](../reference/remocon-rpc.ko.md#listpackageinfo)는
[`ListPackageInfoRequest`](../reference/remocon-rpc.ko.md#listpackageinforequest)를
받습니다:

```rust
let request = ListPackageInfoRequest {
    package_names: vec!["navi".to_string()],
};
let response = client.list_package_info(request).await?;
```

---

## Parse the response

- 모든 method는 동일한 JSON 형식으로 작성된 단일 `result: string` 필드를 반환합니다.
- Method call이 성공하면 `success` 필드가 `true` 이고 `data` 필드가
  있으며, 실패하면 `success` 필드가 `false` 이고 `reason` 필드가
  있습니다. 단, `RemovePackage` 처럼 반환하는 데이터가 없는 RPC는 성공해도
  `data` 가 생략됩니다.
- `data` 필드는 메시지마다 다른 형식으로 정의됩니다.
- 각 메시지별 `data` 에 설정되는 내용은
  [`libssam/data/remocon-responses.schema.json`](../../libssam/data/remocon-responses.schema.json)
  (JSON Schema)에 정의되어 있습니다.
- method별 정확한 response message와 response data payload는 레퍼런스의
  [API at a Glance](../reference/remocon-rpc.ko.md#api-at-a-glance)를
  참고하세요.
- Response message는 다음과 같은 순서로 처리해주세요.
  1. `schema_version` 부터 확인하세요. 모르는 버전이면 `data` 의 형태를 추측하지
     말고 거부하세요.
  2. `success` 를 확인하세요. 실패면 `reason` 을 읽고, 성공이면 `data` 를 해당
     method의
     [response data payload](../reference/remocon-rpc.ko.md#response-data-payload)로
     디코딩하세요.

예를 들어, `ListPackageInfo` method에 대한 response는 다음과 같이 처리해주세요:

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
- 반환되는 [`packageActionResult`](../reference/remocon-rpc.ko.md#packageactionresult)
  배열은 패키지당 한 항목씩 담습니다. 응답 자체의 `success` 가 `true` 여도
  개별 항목은 실패했을 수 있으므로, 각 항목을 따로 확인해야 합니다.

예를 들어:

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

이 RPC 자체엔 TLS나 인증이 없습니다(
[Connection](../reference/remocon-rpc.ko.md#connection) 참조).
- 원격 접근이 꼭 필요한 게 아니라면 데몬의 기본 localhost 바인딩을 유지하고, 노출해야
한다면 네트워크·OS 수준 접근 제어로 범위를 제한하세요(
[`rpc_bind_ip`](../reference/daemon-config.ko.md#rpc_bind_ip) 참조).

- 그렇다고 패키지 진위 검증이 약화되는 것은 아닙니다: 검증은 RPC를 호출하는
주체와 무관하게 독립적으로 강제되므로, 서명되지 않거나 변조된 패키지는
누가 호출하든 설치 단계에서 거부되어 로드·실행되지 않습니다(
[제어 인터페이스 보안](../architecture/package-integrity.ko.md#control-interface-security-rpc) 참조).
