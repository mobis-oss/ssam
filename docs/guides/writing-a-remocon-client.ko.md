# Writing a Remocon Client

<p>
  <a href="writing-a-remocon-client.md"><img src="https://img.shields.io/badge/lang-English-blue" alt="English"></a>
</p>

이 문서는 `ssamd`가 *gRPC* 로 제공하는 **Remocon** API를 사용하여 클라이언트를 작성하는 방법을 설명합니다.
`ssam` CLI도 이 프로토콜로 작성되었습니다.
예시 코드는 Rust(`tonic`)를 사용합니다.

> 📖 전체 프로토콜은 **[Remocon gRPC 제어 API 레퍼런스](../reference/remocon-rpc.ko.md)** 를 참고하세요.

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

이 RPC로 다양한 형태의 클라이언트를 만들 수 있습니다.
예를 들면:

- **컨테이너 lifecycle을 직접 관리하는 container manager** — 자체 정책에 따라 언제 설치·시작·중지·제거할지 스스로 판단.
- **롤아웃을 전담하는 update manager** — 새 버전을 언제 설치할지, downgrade나 rollback 여부를 자체 로직으로 판단.

---

## Get the protocol

서비스는 [`libssam/data/ssam.remocon.proto`](../../libssam/data/ssam.remocon.proto) (proto 패키지 `ssamd.remocon`)에 정의되어 있습니다.
사용하는 언어의 gRPC/protoc 도구로 스텁을 생성하세요 — 코드 생성은 표준 *gRPC* 절차입니다(Python은 `grpc_tools.protoc`, Go는 `protoc-gen-go` 등).

예를 들어 여기서는 `tonic_prost_build::compile_protos`로 Rust 스텁을 생성합니다:

```rust
// build.rs
fn main() -> anyhow::Result<()> {
    tonic_prost_build::compile_protos("data/ssam.remocon.proto")?;
    Ok(())
}
```

---

## Connect to `ssamd`

`ssamd`는 고정 abstract Unix domain socket `@ssamd`에서 로컬 *gRPC* 연결을 받습니다.

채널을 열어 클라이언트를 만듭니다.
데몬이 아직 없을 수 있으니 연결 실패는 잠시 후 재시도합니다:

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

다음 순서로 method를 호출합니다:

1. 레퍼런스의 [API at a Glance](../reference/remocon-rpc.ko.md#api-at-a-glance)에서 호출할 method를 고릅니다.
2. 해당 [request message](../reference/remocon-rpc.ko.md#request-messages)를 채웁니다.
   예를 들어 [`ListPackageInfo`](../reference/remocon-rpc.ko.md#listpackageinfo)는 [`ListPackageInfoRequest`](../reference/remocon-rpc.ko.md#listpackageinforequest)를 받습니다.
3. request에 `x-ssam-api-version` metadata를 붙여 `ApiVersion` enum의 [`CURRENT`](../reference/remocon-rpc.ko.md#x-ssam-api-version)를 값으로 넣습니다.
4. 완성한 request로 method를 호출합니다.

아래는 요청을 만들어 method를 호출하는 예입니다.
`generated::remocon`으로 코드를 생성했다고 가정합니다:

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

> ⚠️ `x-ssam-api-version` 값 주의:
>
> - 숫자를 직접 하드코딩하지 말고 codegen으로 생성된 `ApiVersion` enum 값을 사용하세요.
> - 서버가 요구하는 버전과 일치하지 않으면 request가 gRPC `FAILED_PRECONDITION` 오류로 거부됩니다.

> ⚠️ 호출자의 effective GID가 데몬의 effective GID와 일치하거나 [데몬의 허용 GID 목록](../reference/daemon-config.ko.md#allowed_gids)에 포함돼야 합니다.
> 그렇지 않으면 `ssamd`가 `SO_PEERCRED` 검사에서 RPC를 gRPC `PermissionDenied`로 거부합니다.

> ⚠️ `ListPackageInfo`를 포함하여, `package_names`를 필요로 하는 method는 이 값을 비워(`[]`) 보내면 설치된 **전체** 패키지를 대상으로 합니다.
> 이는 `WipePackageData`처럼 **파괴적인** method에서는 치명적이므로, 별도의 확인 절차나 안전장치가 필요합니다.

---

## Parse the response

각 method는 대응하는 response message를 반환합니다.
호출 자체가 실패하면 *gRPC* `Status`(예: `NotFound`, `FAILED_PRECONDITION`)로 오고, 성공하면 그 message의 필드를 직접 읽습니다.

> 📖 대응하는 response message 및 그 필드는 [Response messages](../reference/remocon-rpc.ko.md#response-messages)·[Response data types](../reference/remocon-rpc.ko.md#response-data-types)를 참고하세요.

### Single-message response

요청의 결과가 단일 message인 method는 그 필드를 바로 읽습니다.

예를 들어 `InstallPackage`의 응답은 [`PackageData`](../reference/remocon-rpc.ko.md#packagedata) 하나를 담으며, 다음과 같이 처리합니다:

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

요청의 결과가 message의 **배열**인 method는 호출 자체가 성공해도 개별 항목은 실패할 수 있으므로, 배열의 **각 항목 `success`** 를 따로 확인해야 합니다.

예를 들어 `ListPackageInfo`의 응답은 [`PackageInfo`](../reference/remocon-rpc.ko.md#packageinfo) 배열을 담으며, 다음과 같이 처리합니다:

```rust
let response = client.list_package_info(request).await?.into_inner();

for package in response.packages {
    if package.success {
        // 정상 패키지 — package_data·quota 등 선택 필드를 읽습니다.
        if let Some(data) = package.package_data {
            println!("{} {} ({})", data.package_name, data.version, package.status);
        }
    } else {
        // 조회 실패 — error_summary·error_details 를 읽습니다.
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

- Remocon 클라이언트의 제어 API 사용은 `ssamd`와 Remocon 클라이언트의 effective GID를 기반으로 허용 여부를 결정하므로, 시스템의 실행환경 구성에서 맞춰줘야 합니다.
- 패키지 진위 검증은 이와 **별개의 보안 계층** 입니다: 허용된 Remocon 클라이언트에서의 설치 요청일지라도 항상 패키지의 서명·무결성 검증을 거치므로, 서명되지 않거나 변조된 패키지는 거부되어 로드·실행되지 않습니다.

> 📖 제어 인터페이스의 접근 통제(로컬 소켓·peer 자격 증명)와 위협 모델은 [제어 인터페이스 보안](../architecture/package-integrity.ko.md#control-interface-security-rpc)을 참고하세요.
