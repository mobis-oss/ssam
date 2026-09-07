# SSAM Package Integrity

<p>
  <a href="package-integrity.md"><img src="https://img.shields.io/badge/lang-English-blue" alt="English"></a>
</p>

SSAM 패키지의 **내용을 신뢰** 하기 위한 세 가지를 다룹니다 — 진짜인지, 온전한지, 바뀌지 않는지.
이 문서는 각각을 운영자와 패키지 작성자가 **어떻게 설정하는지** 설명합니다:

- **패키지 서명** — 검증이 활성화된 경우 유효한 서명이 있는 패키지만 로드합니다.
- **파일시스템 무결성** — 실행 중 블록 변조를 지속적으로 감지합니다.
- **읽기 전용 파일시스템** — 파일시스템 자체를 불변으로 만듭니다.

> 📖 실행 중 컨테이너의 행위 제한(seccomp·AppArmor)은 [Container Hardening](container-hardening.ko.md), 네트워크·자원 격리는 [Container Isolation](container-isolation.ko.md) 가이드를 참고하세요.

> 📖 이 무결성 모델의 설계·위협 모델은 [Package Integrity 설계 문서](../architecture/package-integrity.ko.md)를 참고하세요.

---

## Table of Contents

1. [Package signing — RSA](#package-signing--rsa)
2. [Filesystem integrity — dm-verity](#filesystem-integrity--dm-verity)
3. [Read-only filesystem](#read-only-filesystem)

---

## Package signing — RSA

SSAM 패키지는 **RSA 개인 키** 로 서명되고(`ssam-wrap`), 패키지 서명 검증이 활성화된 경우 디바이스에 설치된 **공개 키** 로 검증됩니다(`ssamd`).
검증에 실패한 패키지는 로드 단계에서 거부됩니다.

> ⚠️ [`debug.disable_package_verify`](../reference/daemon-config.ko.md#disable_package_verify)는 개발·디버그 전용 예외 설정입니다.
> 기본값은 `false`입니다.
> `true`로 설정하면 `ssamd`가 RSA 서명 검증과 공개 키 파일 존재 검사를 건너뜁니다.
> 따라서 공개 키 없이도 패키지를 로드할 수 있습니다.
> 패키지 포맷과 payload 구조 검사는 계속 수행됩니다.
> 운영 환경에서는 이 설정을 `false`로 유지하세요.

> 📖 서명이 무엇을 보장하는지는 [패키지 무결성 → 신뢰 모델](../architecture/package-integrity.ko.md#trust-model)을 참고하세요.

### 키 생성

`ssamd`는 다음 사양의 키를 사용합니다:

- **종류** — *RSA*
- **크기** — 2048비트 이상
- **개인 키 포맷** — PKCS#8 PEM
- **공개 키 포맷** — X.509 SPKI PEM

그리고 다음 방식으로 서명합니다:

- **패딩** — PKCS#1 v1.5
- **해시** — SHA-256

키는 예를 들어 OpenSSL로 다음과 같이 생성합니다:

```console
$ openssl genpkey -algorithm RSA -out private_key.pem -pkeyopt rsa_keygen_bits:2048
$ openssl rsa -in private_key.pem -pubout -out public_key.pem
```

기본 검증 구성에서는 생성한 키를 다음과 같이 사용합니다:

- 개인 키(`private_key.pem`)는 `ssam-wrap --private-key`로 패키지 서명에 씁니다.
- 공개 키(`public_key.pem`)는 디바이스에 설치하고, 데몬 설정 `ssamd.toml`의 [`package.public_key`](../reference/daemon-config.ko.md#public_key)로 지정합니다.

### 개인 키 보호

개인 키는 **서명 권한 그 자체** 입니다 — 이를 가진 누구든 디바이스가 검증할 패키지를 만들 수 있습니다.
HSM이나 접근이 제한된 CI 시크릿에 보관하고, **디바이스로는 절대 배포하지 마세요**.

### 신뢰 체인 보호

패키지 서명 검증이 활성화된 경우 서명에 대한 신뢰는 두 가지에 달려 있습니다 — 검증 기준인 **공개 키** 와, 검증을 실제로 수행하는 **`ssamd` 바이너리** 입니다.
둘 중 하나라도 조작되면 위조 패키지가 통과할 수 있으므로 둘 다 보호해야 합니다:

- **공개 키** — `ssamd`가 신뢰하는 유일한 신뢰 앵커입니다.
  공격자가 자기 키로 바꿔치기하면 위조 패키지가 통과합니다.
- **`ssamd` 바이너리** — 서명 검증을 수행하는 주체입니다.
  검증이 무력화된 `ssamd`로 바꿔치기되면 서명이 틀려도 통과합니다.

패키지 서명 검증이 활성화된 경우 둘 다 **읽기 전용·시큐어 부트로 보호되는 경로** 에 두고 쓰기 권한을 최소화하세요.
시큐어 부트를 지원하는 플랫폼에서는 그 신뢰가 부트체인에서 `ssamd`와 공개 키까지 이어지도록 구성합니다 — 플랫폼이 부트체인을 검증하고, 그 무결성이 `ssamd` 바이너리를 담은 서명된 rootfs와 공개 키가 놓인 읽기 전용 경로를 보호하며, 조작되지 않은 `ssamd`가 공개 키를 신뢰 앵커로 삼아 패키지 서명을 검증합니다:

```mermaid
flowchart LR
  SB["플랫폼 시큐어 부트"]
  BOOT[/"서명된 커널 · rootfs"/]
  PK[/"공개 키<br/>(읽기 전용 경로)"/]
  D["ssamd<br/>(서명 검증 활성화)"]
  PKG(["신뢰된 패키지"])

  SB ==>|"부트체인 검증"| BOOT
  BOOT ==>|"위치 보호"| PK
  BOOT ==>|"포함"| D
  PK ==>|"신뢰 앵커"| D
  D ==>|"서명 검증"| PKG

  classDef actor  fill:#dbeafe,stroke:#2563eb,color:#1e3a5f;
  classDef data   fill:#eef1f5,stroke:#94a3b8,color:#334155;
  classDef ext    fill:#fef3c7,stroke:#d97706,color:#78350f;
  classDef result fill:#dcfce7,stroke:#16a34a,color:#14532d;
  class SB,BOOT ext;
  class PK data;
  class D actor;
  class PKG result;
```

---

## Filesystem integrity — dm-verity

패키지 파일시스템 이미지는 *dm-verity* 로 블록 단위 무결성을 검증받습니다.
빌드 시 `ssam-wrap`이 해시 트리를 만들고, 런타임에 `ssamd`가 *dm-verity* 대상을 구성하면 커널이 읽기마다 그 트리로 블록을 검증합니다.

> 📖 동작 원리는 [패키지 무결성 → dm-verity](../architecture/package-integrity.ko.md#filesystem-integrity--dm-verity)를 참고하세요.

**커널 설정.** *dm-verity* 는 호스트 커널의 device-mapper verity 대상을 사용합니다.
다음 커널 설정이 필요합니다:

- `CONFIG_BLK_DEV_DM` — device-mapper
- `CONFIG_DM_VERITY` — dm-verity 대상

**운영 설정.** 블록 검증은 기본으로 켜져 있어 별도 설정이 없어도 됩니다.
다만 다음 두 가지에 주의하세요:

- `ssamd.toml`의 [`debug.pkgfs_overlay_dir`](../reference/daemon-config.ko.md#pkgfs_overlay_dir)를 **빈 값** 으로 두세요.
  값을 지정하면 쓰기 가능한 overlay가 얹혀 *dm-verity* 무결성이 사실상 무력화됩니다 — 이 overlay는 **디버깅·개발용** 이므로 운영에서는 비웁니다.
> ⚠️ [`debug.disable_pkgfs_integrity`](../reference/daemon-config.ko.md#disable_pkgfs_integrity)는 **디버깅 전용** 예외 설정입니다.
> `true`로 설정하면 *dm-verity* 대상을 만들지 않고 package filesystem을 loop device에서 직접 마운트합니다.
> 기본 package filesystem은 읽기 전용으로 유지되지만 블록 단위 변조 감지는 꺼집니다.
> 패키지 서명 검증은 [`debug.disable_package_verify`](../reference/daemon-config.ko.md#disable_package_verify)로 별도로 제어합니다.
> 운영 환경에서는 기본값(`false`)을 유지하세요.

---

## Read-only filesystem

패키지 파일시스템을 읽기 전용으로 두면 실행 중 rootfs가 변경되지 않아, 서명·*dm-verity* 로 검증한 내용이 그대로 유지됩니다.
SSAM은 두 파일시스템을 지원합니다:

- **EROFS(기본·권장)** — `ssam-wrap`이 기본으로 빌드합니다 ([`--pkgfs-type`](../reference/ssam-wrap.ko.md#package-filesystem-types)).
  *EROFS* 는 **파일시스템 레벨에서 읽기 전용만 지원** 하므로 마운트된 패키지 파일시스템에는 애초에 쓰기 경로가 없고, **압축을 지원** 해 이미지 크기도 줄입니다.
  불변성과 크기 두 면에서 유리하므로 기본값이자 권장입니다.
- **ext4** — 대안으로 지원합니다.
  읽기 전용으로 **마운트** 되어 런타임에 쓰기가 막힙니다.

읽기 전용 파일시스템은 *dm-verity* 와 **직교하는 별도 방어** 입니다.
파일시스템이 쓰기를 막고, 그 위에서 블록이 조작되더라도 *dm-verity* 가 잡아냅니다 — 서로 독립적인 두 방어입니다.

**커널 설정.** *EROFS* 이미지를 마운트하려면 호스트 커널에 다음이 필요합니다:

- `CONFIG_EROFS_FS` — EROFS 파일시스템
