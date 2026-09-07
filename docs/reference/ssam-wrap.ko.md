# ssam-wrap 명령어 레퍼런스

<p>
  <a href="ssam-wrap.md"><img src="https://img.shields.io/badge/lang-English-blue" alt="English"></a>
</p>

`ssam-wrap`은 SSAM 패키지 생성 도구입니다.
*container-native* 애플리케이션의 파일시스템을 — 디렉터리, 미리 빌드된 이미지 파일, 또는 Docker/OCI 컨테이너 이미지 형태로 — 입력받아, `ssamd`가 설치하고 컨테이너로 실행할 수 있는 서명·무결성 보호된 SSAM 패키지를 생성합니다.

> 📖 패키지 생성의 단계별 안내는 **[SSAM 만들기 — 패키지 생성 가이드](../guides/wrapping-a-ssam.ko.md)** 를 참고하세요.

---

## Table of Contents

1. [Requirements](#requirements)
2. [Modes](#modes)
3. [Command-line Reference](#command-line-reference)
4. [The Workspace](#the-workspace)
5. [Package Filesystem Types](#package-filesystem-types)
6. [Signing](#signing)
7. [Troubleshooting](#troubleshooting)

---

## Requirements

`ssam-wrap`은 여러 외부 도구를 실행합니다.
다음 도구들이 설치되어 있고 `PATH`에서 접근 가능해야 합니다:

| 도구 | 용도 | 필요한 경우 |
| --- | --- | --- |
| `erofs-utils` (`mkfs.erofs`) | EROFS 패키지 파일시스템 이미지 빌드 | EROFS 타입으로 빌드 시 (기본값) |
| `lz4` | EROFS용 LZ4 압축 백엔드 | `erofs-lz4` / `erofs-lz4hc` 빌드 시 |
| `e2fsprogs` (`mkfs.ext4`) | ext4 패키지 파일시스템 이미지 빌드 | `-t ext4` 사용 시 |
| `fakeroot` | root 권한 없이 이미지 빌드 | 디렉터리로부터 이미지 빌드 시 |
| `cryptsetup` (`veritysetup`) | dm-verity 해시 트리 생성 | 항상 (wrap 모드) |
| `truncate` | ext4 이미지 크기 사전 할당 | `-t ext4` 사용 시 |
| [`skopeo`](https://github.com/containers/skopeo) (>= 1.15) | 컨테이너 이미지 복사 | Docker/OCI 소스 사용 시 |
| [`umoci`](https://github.com/opencontainers/umoci) | OCI 이미지를 rootfs로 언팩 | Docker/OCI 소스 사용 시 |
| `docker` | 이미지 아키텍처 확인 | `docker-daemon:` 소스에 한해 |

> **참고:** `truncate`는 GNU coreutils의 일부로, 대부분의 Linux 시스템에 이미 존재합니다.

---

## Modes

`ssam-wrap`은 상호 배타적인 두 모드로 동작합니다:

- **prepare 모드** (`--prepare`) — 워크스페이스에 템플릿 설정 파일(`config.toml`, `runtime.json`, `seccomp.json`)을 생성합니다.
  기존 파일은 덮어쓰지 않습니다.
- **wrap 모드** (기본, `--private-key` 필요) — 패키지 파일시스템 이미지 빌드, *dm-verity* 해시 트리 계산, 서명을 거쳐 최종 SSAM 패키지 파일을 만듭니다.

> 📖 각 단계의 상세와 실제 사용법은 [패키지 생성 가이드](../guides/wrapping-a-ssam.ko.md)를 참고하세요.

---

## Command-line Reference

```
ssam-wrap [OPTIONS] <WORKSPACE>
```

### Positional argument

| 인자 | 설명 |
| --- | --- |
| `<WORKSPACE>` | **(필수)** 워크스페이스 디렉터리 경로. 존재하지 않으면 생성됩니다. 상대 경로는 현재 작업 디렉터리 기준으로 해석됩니다. |

### Options

| 옵션 | 긴 형식 | wrap 전용 | 설명 |
| --- | --- | --- | --- |
| `-p <PACKAGE_NAME>` | `--prepare <PACKAGE_NAME>` | — | **prepare 모드** 로 실행: 해당 이름의 패키지를 위한 `config.toml`, `runtime.json`, `seccomp.json` 템플릿을 생성합니다. |
| `-s <SRC>` | `--pkgfs-src <SRC>` | — | 패키지 파일시스템 소스. 로컬 경로(디렉터리 또는 이미지 파일)이거나 지원되는 컨테이너 transport URI(아래 참조)입니다. |
| `-a <ARCH>` | `--src-oci-arch <ARCH>` | — | 컨테이너 transport에서 쓸 소스 이미지 아키텍처: `arm64`(기본) 또는 `amd64`. 컨테이너가 아닌 소스에서는 무시됩니다. |
| `-k <FILE>` | `--private-key <FILE>` | ✓ | 패키지 서명에 사용할 RSA 개인 키. **wrap 모드에서 필수입니다.** |
| `-o <FILE>` | `--output <FILE>` | ✓ | 출력 패키지 경로. 기본값은 `<WORKSPACE>/<name>-<version>.ssam` 입니다. |
| `-t <TYPE>` | `--pkgfs-type <TYPE>` | ✓ | 패키지 파일시스템 이미지 타입. 소스가 디렉터리일 때만 사용됩니다. `ext4`, `erofs`, `erofs-lz4`, `erofs-lz4hc`(기본) 중 하나. |

> ⚠️ **wrap 전용**(✓) 옵션을 `--prepare`와 함께 전달하면 오류가 발생합니다.

### Supported container transports

`--pkgfs-src`는 [skopeo transport](https://github.com/containers/image/blob/main/docs/containers-transports.5.md)의 일부를 지원합니다:

- `docker://` — 원격 레지스트리 이미지 (예: `docker://docker.io/hello-world:latest`)
- `docker-daemon:` — 로컬 *Docker* 데몬의 이미지
- `docker-archive:` — `docker save`로 만든 tar 아카이브
- `oci-archive:` — OCI 이미지 tar 아카이브
- `oci:` — OCI 이미지 레이아웃 디렉터리

`--pkgfs-src`에 전달된 그 외의 값은 **로컬 파일시스템 경로** 로 취급됩니다.

---

## The Workspace

워크스페이스는 단순한 디렉터리입니다.
패키지를 준비하고 빌드한 뒤에는 다음을 포함합니다:

```
<workspace>/
├── config.toml        # 패키지 설정 (메타데이터, 보안, 서비스)
├── runtime.json       # OCI Runtime config (프로세스 인자, 마운트, capability)
├── seccomp.json       # seccomp 정책
├── pkgfs/             # 패키지 파일시스템: 디렉터리, 심링크, 또는 rootfs
├── intermediate/      # 빌드 산출물 (fs 이미지, 루트 해시) — 삭제해도 안전
└── <name>-<version>.ssam   # 최종 패키지 (기본 출력 위치)
```

- `pkgfs/`가 **디렉터리** 이면, `ssam-wrap`은 그것으로부터 파일시스템 이미지를 빌드합니다.
- `pkgfs/`(또는 그 심링크 대상)가 **파일** 이면, 이미 빌드된 이미지로 간주하여 그대로 사용합니다(`--pkgfs-type` 옵션은 무시됨).
- `intermediate/` 디렉터리는 빌드된 이미지와 `.root_hash` 파일을 담습니다.
  빌드 사이에 삭제해도 됩니다.

> 📖 세 개의 설정 파일에 무엇이 들어가고 어떻게 채우는지는 [패키지 생성 가이드](../guides/wrapping-a-ssam.ko.md)와 [`config.toml` 레퍼런스](package-config.ko.md)를 참고하세요.

---

## Package Filesystem Types

`-t` / `--pkgfs-type`으로 선택합니다(소스가 디렉터리일 때만):

| 타입 | 백엔드 | 비고 |
| --- | --- | --- |
| `erofs-lz4hc` | `mkfs.erofs -zlz4hc` | **기본값.** 최고 압축률, 읽기 최적화. |
| `erofs-lz4` | `mkfs.erofs -zlz4` | 빌드가 더 빠르고, lz4hc보다 압축률이 낮음. |
| `erofs` | `mkfs.erofs` | 압축 없는 EROFS. |
| `ext4` | `mkfs.ext4` | 쓰기 가능한 형태의 레이아웃; `truncate`로 이미지 크기를 사전 할당(소스 크기 + 10% 오버헤드, 최소 3 MiB). |

SSAM의 읽기 전용·무결성 검증 런타임 모델에는 *EROFS* 를 권장합니다.

---

## Signing

wrap 모드는 각 패키지를 *RSA* 개인 키로 서명하고, `ssamd`가 대응하는 공개 키로 검증합니다.
개인 키는 `--private-key`로 전달합니다.

성공하면 `ssam-wrap`은 패키지 이름, 서비스 타입, 사용된 서명 키, 서명된 *dm-verity* 테이블을 출력합니다.

> 📖 *RSA* 키 쌍 생성과 개인·공개 키 보호는 [Package Integrity → 패키지 서명](../guides/package-integrity.ko.md#package-signing--rsa)을 참고하세요.

---

## Troubleshooting

| 증상 | 원인 / 해결 |
| --- | --- |
| `Private key file path is required` | `--private-key` 없이 wrap 모드를 실행했습니다. `-k <key>`를 추가하거나, 설정 파일 템플릿만 만들려면 `--prepare`를 사용하세요. |
| `Package filesystem already exists, please delete ...` | 이전 Docker/OCI 추출로 인해 `pkgfs/`가 남아 있습니다. 다시 추출하기 전에 제거하세요. |
| `Required definition of mount '/dev' is missing ...` | `runtime.json`에 필수 마운트가 없습니다. `/dev`, `/dev/pts`(그리고 `notify` 서비스의 경우 `/run`)를 추가하세요. |
| `Docker image ... is not for <arch> architecture` | `docker-daemon:` 이미지의 아키텍처가 `--src-oci-arch`와 다릅니다. 올바른 `-a` 값을 전달하세요. |
| `skopeo` / `umoci` not found | 컨테이너 도구를 설치하세요([Requirements](#requirements) 참조). |
| `WARN: Given package filesystem type would be ignored!` | 소스가 디렉터리가 아니라 이미지 파일로 해석되었습니다. `-t`는 디렉터리 소스에만 적용됩니다. |

> 📖 전체 패키지 생성 워크스루는 [SSAM 만들기 가이드](../guides/wrapping-a-ssam.ko.md)를, `ssamd`에서 패키지를 컨테이너로 실행하는 것까지 포함한 완전한 예제는 [빠른 시작 가이드](../../README.ko.md#quick-start)를 참고하세요.
