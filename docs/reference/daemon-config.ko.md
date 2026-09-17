# ssamd 설정(`ssamd.toml`) 레퍼런스

<p>
  <a href="daemon-config.md"><img src="https://img.shields.io/badge/lang-English-blue" alt="English"></a>
</p>

`ssamd.toml`은 SSAM 데몬(`ssamd`)의 설정 파일입니다.
패키지가 어디에 있고, 어디에 마운트·저장하며, 어떤 공개 키로 패키지 서명을 검증하는지 등을 설정합니다.

> 📖 `ssamd` 설치·설정의 단계별 안내는 **[ssamd 운영 가이드](../guides/serving-ssam.ko.md)** 를 참고하세요.

`ssamd`는 `ssamd.toml`을 **`ssamd` 바이너리와 같은 디렉터리** 에서 로드하며, 명령줄 인자나 환경변수로 이를 재정의하는 수단은 제공하지 않습니다.
데몬이 쓰는 디렉터리뿐 아니라 패키지 서명을 검증하는 공개 키의 위치까지 이 파일이 정하므로, 설정 파일 자체도 보호해야 할 대상입니다 — 변조된 설정으로 데몬이 실행되는 것을 원천 차단하려는 의도된 결정입니다.
따라서 데몬 바이너리는 물론 그 설정 파일까지 시스템의 *신뢰 체인* 으로 보호되는 위치에 두기를 권장합니다.

> **생성 방법:** 이 레퍼런스를 참고해 직접 작성하거나, 소스 혹은 배포 패키지에 포함된 `gen_ssamd_conf.sh` 헬퍼로 생성합니다([ssamd 운영 가이드](../guides/serving-ssam.ko.md#generating-configuration-with-gen_ssamd_confsh) 참조).

---

## Table of Contents

1. [File structure at a glance](#file-structure-at-a-glance)
2. [`[package]`](#package)
3. [`[package.dir]`](#packagedir)
4. [`[debug]`](#debug)
5. [`[network]`](#network)
6. [`[rpc]`](#rpc)

---

## File structure at a glance

`ssamd`는 데몬 바이너리와 같은 디렉터리의 `ssamd.toml`을 읽습니다.

```toml
[package]
extension = "ssam"
cgroup = ""
public_key = "/etc/ssamd/public.key"

[package.dir]
builtin = "/usr/share/ssamd/packages"
downloaded = "/var/lib/ssamd/packages"
oci_bundle = "/var/lib/ssamd/oci-bundle"
data = "/var/lib/ssamd/data"

[debug]
pkgfs_overlay_dir = ""
disable_pkgfs_integrity = false
disable_package_verify = false

[network.bridge]
enabled = false

[network.bridge.addr_pool]
base = "172.20.0.0/16"
size = 29

[rpc]
allowed_gids = []
```

`package.dir.builtin`과 `package.dir.downloaded`는 선택 source입니다. 둘 중
하나는 반드시 비어 있지 않아야 합니다. builtin package는 읽기 전용 system
package입니다. downloaded package는 install, upgrade, remove 대상입니다.
같은 이름이 두 source에 있으면 downloaded package가 우선합니다.

`package.dir.oci_bundle`과 `package.dir.data`는 필수 non-empty 경로입니다.
`package.dir.oci_bundle`은 absolute path여야 합니다.
package source와 runtime directory는 여러 사용자가 공유하는 writable directory
아래에 두지 않아야 합니다.
두 package source는 같은 디렉터리로 resolve되면 안 됩니다.

## `[package]`

### `extension`

점(.)을 제외한 package 파일 확장자입니다. 기본값은 `"ssam"`입니다.

### `cgroup`

package process에 사용할 systemd cgroup입니다. 기본값은 `""`입니다.

### `public_key`

package signature verification에 사용할 RSA public key 경로입니다. 기본값은 `/etc/ssamd/public.key`입니다.

`debug.disable_package_verify = false`이면 `public_key`는 비어 있으면 안
됩니다. signature verification이 활성화된 startup에서는 key file도
존재해야 합니다. 빈 key는 verification을 명시적으로 끈 경우에만 허용됩니다.

## `[package.dir]`

### `builtin`

선택 read-only package source입니다. 빈 값은 source를 끕니다.
디렉터리는 `ssamd` 실행 전에 존재해야 하며 데몬이 생성하지 않습니다.

### `downloaded`

선택 read-write package source입니다. 빈 값이면 install이 설정 오류가 됩니다.
설정하면 데몬이 startup 때 디렉터리를 생성합니다.
`InstallPackage`는 이 source가 설정된 경우에만 사용할 수 있으므로 builtin-only
배포에서는 runtime package 설치 요청이 설정 오류로 거부됩니다.

### `oci_bundle`

version별 OCI bundle root입니다. 필수입니다.

package 이름이 `name`, version이 `version`이면 layout은 다음과 같습니다.

```text
oci_bundle/
└── name_version/
    ├── config.json
    └── pkgfs/
```

`config.json`은 OCI runtime 설정입니다. `pkgfs/`는 package filesystem mount target이며 OCI `root.path`는 absolute 경로입니다.
데몬은 startup 때 이 디렉터리를 생성합니다.

### `data`

container data directory root입니다. 필수입니다.
데몬은 startup 때 이 디렉터리를 생성합니다.

## `[debug]`

### `pkgfs_overlay_dir`

선택 package filesystem overlay root입니다. 빈 값이면 overlay를 끕니다.

### `disable_pkgfs_integrity`

dm-verity enforcement를 끕니다. 기본값은 `false`입니다.

### `disable_package_verify`

RSA signature verification만 끕니다. 기본값은 `false`입니다.

verification을 꺼도 package format, footer, payload bounds, metadata, runtime
configuration, seccomp policy, filesystem payload 검증은 계속 수행합니다.
malformed package는 항상 거부합니다.
`disable_pkgfs_integrity`는 signature verification과 독립적입니다.

## `[network]`

데몬이 관리하는 network 설정입니다.
섹션 전체는 선택입니다.

### `[network.bridge]`

`enabled`는 데몬 관리 bridge networking을 제어합니다.
기본값은 `false`입니다.

### `[network.bridge.addr_pool]`

선택 address pool은 부모 bridge table과 함께 설정합니다:

```toml
[network.bridge]
enabled = true

[network.bridge.addr_pool]
base = "172.20.0.0/16"
size = 29
```

선택 address pool 기본값은 `172.20.0.0/16`이며 container subnet 기본 prefix는
`/29`입니다.
`/30`보다 긴 prefix는 usable host가 없어 거부합니다.

## `[rpc]`

`allowed_gids`는 Unix RPC socket 접근을 허용할 추가 peer effective 또는 supplementary GID 목록입니다.
peer의 effective 또는 supplementary GID가 데몬의 effective GID와 일치하면 항상 허용됩니다. 기본값은 `[]`입니다.
