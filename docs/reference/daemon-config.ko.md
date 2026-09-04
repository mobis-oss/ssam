# ssamd 설정(`ssamd.toml`) 레퍼런스

<p>
  <a href="daemon-config.md"><img src="https://img.shields.io/badge/lang-English-blue" alt="English"></a>
</p>

`ssamd.toml` 은 SSAM 데몬(`ssamd`)의 설정 파일입니다. 패키지가 어디에 있고,
어디에 마운트·저장하며, 어떤 공개 키로 패키지 서명을 검증하는지 등을 설정합니다.

> `ssamd` 설치·설정의 단계별 안내는 **[ssamd 운영 가이드](../guides/serving-ssam.ko.md)** 를
> 참고하세요. 이 문서는 설정 키 **레퍼런스** 입니다.

`ssamd` 는 `ssamd.toml` 을 **`ssamd` 바이너리와 같은 디렉터리** 에서 로드하며,
명령줄 인자나 환경변수로 이를 재정의하는 수단은 제공하지 않습니다. 데몬이 쓰는
디렉터리뿐 아니라 패키지 서명을 검증하는 공개 키의 위치까지 이 파일이 정하므로,
설정 파일 자체도 보호해야 할 대상입니다 — 변조된 설정으로 데몬이 실행되는 것을
원천 차단하려는 의도된 결정입니다. 따라서 데몬 바이너리는 물론 그 설정 파일까지
시스템의 *신뢰 체인* 으로 보호되는 위치에 두기를 권장합니다.

> **생성 방법:** 이 레퍼런스를 참고해 직접 작성하거나, 소스 혹은 배포 패키지에 포함된 `gen_ssamd_conf.sh`
> 헬퍼로 생성합니다([ssamd 운영 가이드](../guides/serving-ssam.ko.md#generating-configuration-with-gen_ssamd_confsh) 참조).

---

## Table of Contents

1. [File structure at a glance](#file-structure-at-a-glance)
2. [`[common]`](#common)
3. [`[rpc]`](#rpc)
4. [`[network]`](#network)
5. [`[integrity]`](#integrity)
6. [Startup validation](#startup-validation)
7. [Complete examples](#complete-examples)

---

## File structure at a glance

```toml
[common]
bundled_packages_dir    = "/var/lib/ssamd/bundled"
downloaded_packages_dir = "/var/lib/ssamd/downloaded"
packages_data_root      = "/var/lib/ssamd/data"
packages_overlayfs_root = ""                          # 비워도 됨
packages_mnt_root       = "/var/run/ssamd/mnt"
public_key_file_path    = "/etc/ssamd/public.key"
packages_cgroup         = ""                          # 비워도 됨
packages_ext            = "ssam"

[network.bridge]                                      # 선택 섹션
enabled = true

[network.bridge.addr_pool]                            # 선택
base = "172.20.0.0/16"                                # 기본값
size = 29                                             # 기본값

[rpc]                                                  # 선택 섹션
allowed_gids = []                                      # 기본값

[integrity]                                           # 선택 섹션
disable_runtime_integrity = false                     # 기본값
```

`[common]` 의 모든 키는 **반드시 존재해야** 합니다. 그렇지 않으면 데몬이 기동에
실패합니다. `[rpc]`, `[network]`, `[integrity]` 섹션은 **선택** 입니다.

---

## `[common]`

핵심 데몬 경로와 설정. 모든 키는 문자열입니다.

### `bundled_packages_dir`

- **필수.** **bundled**(읽기 전용, 사전 설치) SSAM 패키지가 있는 디렉터리.
  미리 존재해야 하며, 없으면 `ssamd` 가 기동 시 중단됩니다.

### `downloaded_packages_dir`

- **필수.** **downloaded**(읽기·쓰기, 런타임 설치) SSAM 패키지가 있는
  디렉터리. 없으면 기동 시 자동으로 생성됩니다.

### `packages_data_root`

- **필수.** 각 패키지의 영속적·쓰기 가능한 **데이터 디렉터리**(패키지의
  [`data_dirs`](package-config.ko.md#data_dirs) 참조)가 저장되는
  루트 디렉터리. *ext4* 프로젝트 쿼터(`storage_limit`)도 여기서 적용됩니다.

### `packages_overlayfs_root`

- **존재는 필수** 이나 **빈 문자열**(`""`)이어도 됩니다.
- *dm-verity* 로 검증되는 읽기 전용 패키지 파일시스템 위에 읽기·쓰기 레이어를 얹어,
  파일을 수정해가며 개발·테스트할 수 있게 하는 편의 설정입니다. 읽기·쓰기로 마운트할
  적당한 위치를 지정합니다.
- 컨테이너는 이 overlay의 merged 뷰로 실행되어 upper에 덮어쓴 파일을 그대로 사용하므로,
  *dm-verity* 가 보장하는 무결성이 사실상 무력화됩니다. 운영 환경에서는 빈 값으로
  두어 비활성화하세요.

### `packages_mnt_root`

- **필수.** *dm-verity* 검증 후 패키지 파일시스템(`pkgfs`)이 런타임에 마운트되는
  루트 디렉터리.

### `public_key_file_path`

- **필수.** 패키지 서명 검증에 사용하는 **RSA 공개 키** 경로. 패키지를 서명할 때
  `ssam-wrap --private-key` 에 전달한 개인 키와 짝이 맞아야 합니다 —
  [서명과 무결성](../architecture/package-format.ko.md#signature-and-integrity) 참조. 다른
  키로 서명된 패키지는 거부됩니다.
- 이 경로가 실제 파일을 가리키지 않으면 `ssamd` 는 **기동 시 중단** 됩니다.

### `packages_cgroup`

- **존재는 필수** 이나 **빈 문자열**(`""`)이어도 됩니다.
- 패키지 컨테이너가 놓이는 cgroup 이름(예: `ssamd.slice` 같은 systemd 슬라이스).
- `ssamd` 는 cgroup을 생성하지 않으므로, 값을 설정했다면 데몬 기동 이전에 해당
  cgroup이 **이미 존재해야** 하며, 없으면 데몬이 중단됩니다.
- 컨테이너를 특정 cgroup에 두지 않으려면 비워 두세요.

### `packages_ext`

- **필수.** 앞의 점을 제외한 패키지 파일 확장자(예: `ssam`). 이 확장자를 가진
  파일이 SSAM 패키지로 인식됩니다.

### RPC 엔드포인트

데몬의 RPC 제어 서버는 Linux abstract Unix domain socket `@ssamd`에서 수신합니다.
엔드포인트는 고정되어 있어 설정할 수 없습니다. abstract socket은 파일시스템 경로가
아니므로 socket 파일이나 runtime directory를 만들지 않습니다. RPC 클라이언트는 같은
시스템 환경에서 실행되어야 하며 peer credential 검사를 통과해야 합니다. peer의
effective GID가 데몬의 effective GID와 일치하거나 `[rpc].allowed_gids`에 포함되어야
하며 UID는 확인하지 않습니다.

## `[rpc]`

RPC peer authorization 설정입니다. 섹션 전체를 **생략**하거나 `allowed_gids`를 비워
두면 데몬의 effective GID와 같은 peer만 허용합니다.

### `allowed_gids`

- **부호 없는 정수 배열, 선택(기본 `[]`).** RPC 서버 접근을 추가로 허용할 peer의
  effective GID입니다. 예:

  ```toml
  [rpc]
  allowed_gids = [1000, 1001]
  ```

- 데몬 자신의 effective GID는 이 목록과 무관하게 항상 허용됩니다. 인증에는 UID를
  사용하지 않습니다. peer의 effective GID가 데몬의 effective GID와 목록의 어느
  항목에도 해당하지 않으면 거부됩니다.

---

## `[network]`

데몬 관리 **브리지 네트워크** 설정. 섹션 전체가 선택이며, 관리 브리지 없이
실행하려면 완전히 생략하세요.

이 섹션은 **오직** 네트워크 모드가 `bridge` 인 패키지([`[container.network] mode = "bridge"`](package-config.ko.md#containernetwork))의
네트워크 구성에만 영향을 줍니다. `host` 또는 `none` 네트워킹을 사용하는 패키지는
이 섹션과 무관하게 동작합니다. 데몬 브리지가 비활성인데 `bridge` 를 요청한 패키지는
실행이 **거부** 됩니다 — 잘못된 설정이 정상 동작으로 오인되는 것을 미리 막기 위함입니다.

각 브리지는 **내부** 브리지입니다: 연결된 컨테이너끼리는 통신하지만 게이트웨이·
NAT·외부 라우팅이 없습니다. 브리지 인터페이스 이름은 데몬이 스스로 정합니다.

### `[network.bridge]`

- **`enabled`** — **boolean, 필수.** 데몬 관리 브리지 네트워크 활성화 여부.

### `[network.bridge.addr_pool]`

선택. 브리지별 서브넷을 잘라내는 주소 풀을 정의합니다. 생략하면 기본값을
사용합니다.

- **`base`** — **string, 선택.** 풀 기준 CIDR(IPv4 표기, 예: `172.20.0.0/16`).
  기본값: `172.20.0.0/16`.
- **`size`** — **integer, 선택.** 브리지별 서브넷 prefix 길이. `/30` 이상(수치상
  30 이하)이어야 합니다. 기본값: `29`.

> 브리지 네트워킹은 호스트에 브리지 / veth / network namespace 커널 기능이
> 필요합니다. [운영 가이드](../guides/serving-ssam.ko.md#networking-bridge-mode)를 참고하세요.

---

## `[integrity]`

런타임 패키지 파일시스템 무결성 설정. 섹션 전체가 **선택** 이며, 생략하면 런타임
무결성이 **활성** 상태(기본)로 유지됩니다.

### `disable_runtime_integrity`

- **boolean, 선택(기본 `false`).** `true` 면 런타임에 커널 *dm-verity* 블록 검증을
  건너뜁니다 — 패키지 파일시스템이 verity 타깃 없이 마운트됩니다. 이 설정과
  무관하게 **패키지 서명 검증은 계속 적용** 되며, 읽기마다 수행되는 블록 검증만
  꺼집니다. 지속적 무결성을 유지하려면 `false` 로 두거나 섹션을 생략하세요.
  [패키지 무결성](../architecture/package-integrity.ko.md#filesystem-integrity--dm-verity) 참조.

---

## Startup validation

기동 시 `ssamd` 는:

- 바이너리와 동일 위치에 `ssamd.toml` 이 존재하기를 **요구** 하며, 모든 필수 `[common]`
  키가 있어야 합니다 — 없으면 설정 오류로 기동에 실패합니다.
- `bundled_packages_dir` 에 지정된 디렉터리가 미리 존재하기를 **요구** 합니다(없으면 중단).
- `public_key_file_path` 에 지정된 파일이 존재하기를 **요구** 하며, 없으면 *"Public key file not found"* 로 중단합니다.
- `downloaded_packages_dir`·`packages_data_root`·`packages_mnt_root` 디렉터리는 없으면 자동으로 **생성** 합니다.
- `packages_cgroup` 이 (비어 있지 않을 때) 존재하는지 **확인** 하고, 없으면 중단합니다.
- RPC 서버를 고정 abstract Unix socket `@ssamd`에 **바인딩** 합니다.
- listener를 바인딩한 뒤 `sd_notify` 를 호출합니다. `ssamd` 를 systemd의 `notify` 타입
  서비스 유닛으로 구성했다면 이때 **준비 완료가 통지** 됩니다.

---

## Complete examples

### Minimal (no managed network)

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

### With cgroup confinement and bridge networking

```toml
[common]
bundled_packages_dir    = "/var/lib/ssamd/bundled"
downloaded_packages_dir = "/var/lib/ssamd/downloaded"
packages_data_root      = "/var/lib/ssamd/data"
packages_overlayfs_root = "/var/lib/ssamd/data/overlayfs"
packages_mnt_root       = "/var/run/ssamd/mnt"
public_key_file_path    = "/etc/ssamd/public.key"
packages_cgroup         = "ssamd.slice"
packages_ext            = "ssam"

[network.bridge]
enabled = true

[network.bridge.addr_pool]
base = "172.20.0.0/16"
size = 29
```
