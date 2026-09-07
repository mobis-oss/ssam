# SSAM 실행하기
#### ssamd Operations Guide

<p>
  <a href="serving-ssam.md"><img src="https://img.shields.io/badge/lang-English-blue" alt="English"></a>
</p>

[SSAM 패키지를 만들고 나면](wrapping-a-ssam.ko.md), 이를 실행하는 것이 **`ssamd`** 입니다: SSAM 데몬은 패키지를 설치하고, 서명을 검증하고, *dm-verity* 무결성 검사를 구성하고, 패키지 파일시스템을 마운트하고, *container-native* 애플리케이션을 *systemd* 하에서 컨테이너로 실행합니다.

이 가이드는 `ssamd`의 빌드·배포·설정·실행을 안내합니다.

---

## Table of Contents

1. [What ssamd does](#what-ssamd-does)
2. [Requirements](#requirements)
3. [Step 1 — Build `ssamd`](#step-1--build-ssamd)
4. [Step 2 — Prepare directories](#step-2--prepare-directories)
5. [Step 3 — Prepare the public key](#step-3--prepare-the-public-key)
6. [Step 4 — Prepare `ssamd.toml`](#step-4--prepare-ssamdtoml)
7. [Step 5 — Run under systemd](#step-5--run-under-systemd)
8. [Step 6 — Verify](#step-6--verify)
9. [Optional configuration](#optional-configuration)
10. [Generating configuration with `gen_ssamd_conf.sh`](#generating-configuration-with-gen_ssamd_confsh)
11. [Troubleshooting](#troubleshooting)

---

## What ssamd does

`ssamd`는 디바이스에서 SSAM 패키지의 전체 생명주기를 관리하는 상주 데몬입니다:

- 각 패키지를 **로드·검증** 합니다 — 패키지를 열어 페이로드를 파싱하고, 설정된 공개 키로 *RSA* 서명을 확인합니다.
- 패키지의 서명된 *dm-verity* 파라미터로 *dm-verity* 타깃을 구성하여 **무결성을 확립** 하며, 변조가 읽기 시점에 지속적으로 감지됩니다.
- 패키지 파일시스템을 읽기 전용으로 **마운트** 합니다.
- 패키지의 `runtime.json` 으로부터 **OCI Runtime config를 생성** 합니다.
- *OCI Runtime*(`crun`)을 이용해 컨테이너를 *systemd* 서비스로 **실행** 하며, 패키지의 런타임 설정·seccomp 정책·MAC 격리를 적용합니다.
- 각 패키지의 영속 데이터 디렉터리(선택적 *ext4* project quota 제한 포함)와 네트워크(host / none / bridge)를 **프로비저닝** 합니다.
- `ssam` CLI가 패키지를 조회·설치·시작·중지·제거하는 데 사용하는 RPC 제어 엔드포인트(고정 abstract Unix socket `@ssamd`)를 **노출** 합니다.

`ssamd` 자체는 **root** 권한의 **systemd `notify`** 서비스로 실행하는 것을 지원합니다.

---

## Requirements

### Build requirements

- Rust 1.88.0+ (2024 edition).

### Runtime requirements (on the target device)

**도구:**

- [`crun`](https://github.com/containers/crun) — 컨테이너를 실행하는 *OCI Runtime*.
- **systemd** — `ssamd`는 컨테이너를 *systemd* service unit 형태로 만들어 실행합니다.

**필수 커널 기능:**

- dm-verity
- 컨테이너 지원 (namespace, cgroup)
- EROFS

**선택 커널 기능:**

- ext4 project quota — 패키지 `storage_limit` 적용용.
- 브리지 네트워킹 (`bridge` / `veth`).

---

## Step 1 — Build `ssamd`

네이티브 빌드/설치:

```console
$ cargo install --path ssamd
```

`ssamd` 바이너리가 `~/.cargo/bin`에 설치됩니다.

혹은 크로스 컴파일할 수 있습니다.
예를 들어 다음과 같이 musl을 이용한 aarch64용 정적 링크 바이너리를 빌드합니다:

```console
$ cargo build -p ssamd --release --target aarch64-unknown-linux-musl
# 이후 target/aarch64-unknown-linux-musl/release/ssamd 를 사용
```

---

## Step 2 — Prepare directories

`ssamd`를 설정하기 전에, 데몬이 사용할 디렉터리를 정하고 준비합니다.
여기서 정한 경로는 이후 설정을 생성할 때 그대로 사용합니다.

| 디렉터리 | 역할 | 존재하지 않는 경우 |
| --- | --- | --- |
| `bundled_packages_dir` | 읽기 전용, 사전 설치 패키지 | 데몬 실패 |
| `downloaded_packages_dir` | 읽기·쓰기, 런타임 설치 패키지 | 자동 생성 |
| `packages_data_root` | 패키지별 영속 데이터 (+ 쿼터) | 자동 생성 |
| `packages_mnt_root` | 검증된 패키지 파일시스템 이미지가 마운트되는 곳 | 자동 생성 |
| `packages_overlayfs_root` | 오버레이 파일시스템 루트 (선택) | 자동 생성 |

표의 `bundled_packages_dir`는 미리 존재해야 하고 `ssamd`는 이를 생성하지 않습니다.
나머지는 없으면 기동 시 자동 생성되지만, **모두 미리 생성해 두기를 권장** 합니다 — 운영 환경에서는 이 디렉터리들을 특정 파일시스템의 마운트 포인트로 두는 경우가 많고(예: `packages_data_root`는 *ext4* project quota를 지원하는 파일시스템을 마운트하는 것을 전제로 합니다), 마운트 지점이 놓이는 상위 경로가 읽기 전용이면 `ssamd`가 실행 시점에 그 디렉터리를 만들려다 권한 오류로 실패할 수 있기 때문입니다.

예를 들어 다음 경로로 준비합니다:

```console
# mkdir -p \
      /var/lib/ssamd/bundled \
      /var/lib/ssamd/downloaded \
      /var/lib/ssamd/data \
      /var/run/ssamd/mnt
```

`bundled_packages_dir` 에는 기기에 미리 넣어 둘 패키지를 배치합니다.
예를 들어 [SSAM 만들기 가이드](wrapping-a-ssam.ko.md)로 만든 SSAM 패키지를 이 디렉터리에 복사해 둡니다:

```console
# cp helloworld.ssam /var/lib/ssamd/bundled/
```

---

## Step 3 — Prepare the public key

패키지 검증에 사용하는 공개 키를 설치합니다.
이는 `ssam-wrap`으로 패키지를 서명하는 데 사용한 *RSA* 키 쌍의 공개 부분입니다:

```console
# mkdir -p /etc/ssamd
# cp public_key.pem /etc/ssamd/public.key
```

이 공개 키는 `ssamd`가 신뢰하는 유일한 검증 기준이라, 전체 신뢰가 그 무결성에 달려 있습니다 — 공격자가 이를 자기 키로 바꿔치기하면 위조 패키지가 통과합니다.
**읽기 전용·시큐어 부트로 보호되는 경로에 두고 쓰기 권한을 엄격히 제한** 하세요.

> 📖 키 쌍 생성·관리와 시큐어 부트 trust chain은 [Package Integrity → 패키지 서명](package-integrity.ko.md#package-signing--rsa)을, 서명 형식은 [패키지 포맷 → 서명과 무결성](../architecture/package-format.ko.md#signature-and-integrity)을 참고하세요.

---

## Step 4 — Prepare `ssamd.toml`

`ssamd`는 바이너리와 같은 디렉터리의 `ssamd.toml`을 읽습니다(데몬이 읽는 유일한 위치).
이 파일은 [`ssamd.toml` 설정 레퍼런스](../reference/daemon-config.ko.md)를 참고해 직접 작성할 수도 있지만, 편의를 위해 SSAM 은 생성 스크립트를 제공합니다.
앞 단계에서 정한 경로를 인자로 [`gen_ssamd_conf.sh`](#generating-configuration-with-gen_ssamd_confsh) 를 실행해 최소 설정을 생성합니다(파일은 `--output-path`로 지정한 위치에 생성되며, `ssamd` 바이너리와 같은 디렉터리에 두어야 합니다):

```console
$ ssamd/releasedata/gen_ssamd_conf.sh \
      --output-path . \
      --bundled-pkgs-dir /var/lib/ssamd/bundled \
      --downloaded-pkgs-dir /var/lib/ssamd/downloaded \
      --pkgs-data-root /var/lib/ssamd/data \
      --pkgs-mnt-root /var/run/ssamd/mnt \
      --public-key-file /etc/ssamd/public.key
```

생성되는 `ssamd.toml`은 다음과 같으며, 직접 작성해도 됩니다:

```toml
[common]
bundled_packages_dir    = "/var/lib/ssamd/bundled"
downloaded_packages_dir = "/var/lib/ssamd/downloaded"
packages_data_root      = "/var/lib/ssamd/data"
packages_overlayfs_root = ""
packages_mnt_root       = "/var/run/ssamd/mnt"
public_key_file_path    = "/etc/ssamd/public.key"
packages_cgroup         = ""
packages_ext            = "ssam"
```

`[common]`의 모든 키(`bundled_packages_dir` 제외)는 반드시 존재해야 합니다.

빈 값으로 둘 수 있는 다음 키는 **선택 기능** 의 스위치이며, 준비 방법은 [Optional configuration](#optional-configuration)에서 설명합니다:

- `packages_cgroup` — 비우면 컨테이너를 전용 cgroup slice에 배치하지 않습니다.
- `packages_overlayfs_root` — 비우면 오버레이가 비활성(운영 기본값)입니다.

> 📖 각 키의 의미와 기본값, 그리고 그 밖의 모든 설정 항목은 [`ssamd.toml` 설정 레퍼런스](../reference/daemon-config.ko.md)를, 헬퍼의 모든 옵션은 [Generating configuration](#generating-configuration-with-gen_ssamd_confsh)을 참고하세요.

---

## Step 5 — Run under systemd

`ssamd`는 **root** 권한으로 실행되어야 하며, *systemd* 의 `sd_notify`로 준비 상태를 알리므로 가능하면 *systemd* **`notify`** 서비스로 실행하기를 추천합니다.
다만 필수는 아닙니다.

빠른 테스트를 위해 직접(포그라운드) 실행:

```console
# ./ssamd
[INFO] Successfully loaded runtime configuration from '.../ssamd.toml'
...
```

서비스로 실행하려면 `ssamd.toml`이 있는 디렉터리에서 바이너리를 실행하는 `Type=notify` unit을 사용하세요.
[`gen_ssamd_conf.sh`](#generating-configuration-with-gen_ssamd_confsh) 헬퍼가 그에 맞는 `ssamd.service`도 생성해 줍니다.

---

## Step 6 — Verify

데몬이 시작되고 패키지가 인식되는지 확인합니다:

```console
$ ssam list
List of packages
Package name         Status
===============================================
helloworld           Ready
```

패키지 컨테이너는 *systemd* 서비스로 실행되므로, 그 stdout/stderr와 생명주기는 패키지 unit 아래 `journalctl`에 나타납니다.
예를 들어 `helloworld` 패키지의 로그는:

```console
$ journalctl --no-pager -u helloworld
systemd[1]: Started helloworld.service.
...
systemd[1]: helloworld.service: Deactivated successfully.
```

> 📖 `ssam` CLI를 사용한 패키지 관리(`ssam list`/`start`/`stop`/`install`/`remove`)는 [`ssam` CLI 레퍼런스](../reference/ssam.ko.md)를 참고하세요.

---

## Optional configuration

다음 기능들은 기본 구동에 필수가 아니며, 운영 정책에 맞게 설정하는 기능들입니다.
사용하려면 호스트 시스템의 준비가 별도로 필요합니다.

### Storage quota (`storage_limit`)

패키지의 `container.storage_limit`을 강제하려면 `packages_data_root`가 놓인 파일시스템이 *ext4* project quota를 지원해야 합니다.

> 📖 요건과 설정은 [컨테이너 격리 → Storage quota](container-isolation.ko.md#storage-quota-storage_limit)를 참고하세요.

### Container cgroup (`packages_cgroup`)

`packages_cgroup`을 지정하면 `ssamd`가 각 컨테이너를 지정한 cgroup 아래에 배치합니다.
지정한 cgroup은 `ssamd` 시작 전에 존재해야 합니다.

> 📖 systemd slice / 별개 cgroup 설정 방법은 [컨테이너 격리 → Container cgroup](container-isolation.ko.md#container-cgroup-packages_cgroup)을 참고하세요.

### AppArmor confinement for `ssamd`

`ssamd` 데몬 자체를 *AppArmor* 로 구속하려면, 데몬 바이너리 경로에 연결되는 프로파일을 호스트 시스템이 작성해 로드해야 합니다 — `ssamd` 런타임은 자신에게 프로파일을 적용하지 않습니다(컨테이너에 적용하는 MAC은 [Container Hardening → 강제 접근 제어](container-hardening.ko.md#mandatory-access-control--apparmor)에서 다룹니다).

`ssamd` 데몬을 구속하는 프로파일과, `ssamd`가 실행하는 컨테이너의 기본값으로 쓰이는 `container-default` 프로파일은 모두 **운영자가 작성해 대상 디바이스에 로드하는 것이 기본** 입니다.
[`gen_ssamd_conf.sh`](#generating-configuration-with-gen_ssamd_confsh)의 `--gen-apparmor-profile`은 이 둘을 한 파일에 생성하며 — 그대로 써도 되고 필요에 맞게 수정해 쓰는 **템플릿** 으로 삼아도 됩니다:

- **`ssamd` 데몬 프로파일** — 데몬 바이너리 경로에 연결되어 데몬 자체를 구속합니다.
- **`container-default` 프로파일** — 전용 프로파일이 없는 패키지 컨테이너의 폴백입니다.

생성한 프로파일을 대상 디바이스의 *AppArmor* 로 로드하세요.

### Networking

패키지의 `host`와 `none` 네트워크 모드는 데몬 설정 없이 동작합니다.
패키지가 `bridge` 모드를 선택했다면 `ssamd.toml`의 `[network.bridge]`로 데몬의 브리지 지원을 켜야 하며, 호스트 시스템에도 `bridge` / `veth` 커널 기능이 활성화되어 있어야 합니다.

```toml
[network.bridge]
enabled = true
```

> 📖 상세 설정은 [컨테이너 격리 → Bridge networking](container-isolation.ko.md#bridge-networking)을 참고하세요.

---

## Generating configuration with `gen_ssamd_conf.sh`

`ssamd/releasedata/gen_ssamd_conf.sh`는 명령줄 옵션을 받아 합리적인 기본값을 채워 `ssamd.toml`(및 요청 시 *systemd* service unit과 *AppArmor* 프로파일)을 생성하는 **선택적 헬퍼** 입니다.
반드시 필요한 것은 아니며 — 손으로 작성한 `ssamd.toml`도 동작합니다 — 타깃 디바이스를 일관되게 프로비저닝할 때 편리합니다.

인자 없이 실행하면 현재 디렉터리에 기본 `ssamd.toml`을 생성하고, 옵션으로 커스터마이즈할 수 있습니다:

| 옵션 | 용도 |
| --- | --- |
| `--output-path PATH` | 생성 파일이 기록될 위치 (기본: 현재 디렉터리) |
| `--gen-service-unit` | `ssamd.service`(systemd unit)도 생성 |
| `--gen-apparmor-profile` | ssamd용 AppArmor 프로파일도 생성 |
| `--ssamd-install-path PATH` | 타깃 디바이스의 ssamd 설치 경로 |
| `--pkgs-mnt-root PATH` | → `packages_mnt_root` |
| `--bundled-pkgs-dir PATH` | → `bundled_packages_dir` |
| `--downloaded-pkgs-dir PATH` | → `downloaded_packages_dir` |
| `--pkgs-data-root PATH` | → `packages_data_root` |
| `--pkgs-overlayfs-root PATH` | → `packages_overlayfs_root` |
| `--pkgs-cgroup NAME` | → `packages_cgroup` |
| `--public-key-file PATH` | → `public_key_file_path` |
| `--packages-ext EXT` | → `packages_ext` (앞의 점 없이) |
| `--rpc-allowed-gids LIST` | → `rpc.allowed_gids` |
| `--bridge-enabled BOOL` | → `network.bridge.enabled` |
| `--bridge-pool-base CIDR` | → `network.bridge.addr_pool.base` |
| `--bridge-pool-size BITS` | → `network.bridge.addr_pool.size` |
| `--disable-runtime-integrity BOOL` | → `integrity.disable_runtime_integrity` |

예 — 설정, service unit, AppArmor 프로파일을 함께 생성:

```console
$ ssamd/releasedata/gen_ssamd_conf.sh \
      --output-path . \
      --gen-service-unit \
      --gen-apparmor-profile \
      --bundled-pkgs-dir /var/lib/ssamd/bundled \
      --downloaded-pkgs-dir /var/lib/ssamd/downloaded \
      --pkgs-data-root /var/lib/ssamd/data \
      --pkgs-mnt-root /var/run/ssamd/mnt \
      --public-key-file /etc/ssamd/public.key \
      --pkgs-cgroup ssamd.slice
```

> 생성기는 `[network.bridge]`/`[network.bridge.addr_pool]` 섹션을 항상 생성합니다(기본값 비활성화) — 위 플래그로 값을 지정하거나, 이후 `ssamd.toml`을 직접 편집해 바꾸세요.

정확한 옵션 목록은 `gen_ssamd_conf.sh --help`로 확인하세요.

---

## Troubleshooting

| 증상 | 원인 / 해결 |
| --- | --- |
| 기동 시 `Failed to read config file ...` | 바이너리 디렉터리에 `ssamd.toml`이 없거나 읽을 수 없습니다. |
| `Failed to parse config file ...` | 필수 `[common]` 키가 없거나 형식이 잘못되었습니다. [레퍼런스](../reference/daemon-config.ko.md#common)와 대조하세요. |
| `Cgroup ... does not exist` | `packages_cgroup`이 설정되었으나 기동 전에 cgroup/slice가 생성되지 않았습니다. 생성하거나([Optional configuration](#optional-configuration) 참조) 필드를 비워 두세요. |
| 모든 패키지가 서명/검증 오류를 표시 | `public_key_file_path`가 패키지 서명에 사용된 키와 일치하지 않습니다. |
| `bridge` 모드 패키지가 `bridge network mode requested but the daemon network is disabled`로 실패 | `[network.bridge]` 섹션이 없거나 `enabled = false` 입니다. 추가/활성화하세요([Networking](#networking) 참조). |
| 패키지 컨테이너가 `journalctl`에 나타나지 않음 | 컨테이너의 `service_type`과, 타깃에 systemd/`crun`이 있는지 확인하세요. |

> 📖 도구 빌드, 키 생성, 샘플 패키지 생성 및 실행까지 포함한 전체 quick-start는 [빠른 시작 가이드](../../README.ko.md#quick-start)를 참고하세요.
