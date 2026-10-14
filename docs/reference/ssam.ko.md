# ssam CLI 레퍼런스

<p>
  <a href="ssam.md"><img src="https://img.shields.io/badge/lang-English-blue" alt="English"></a>
</p>

`ssam`은 SSAM 데몬(`ssamd`)의 명령줄 클라이언트입니다.
`ssamd`의 [Remocon gRPC API](remocon-rpc.ko.md)를 호출해 실행 중인 데몬의 패키지를 설치·시작·중지·제거·조회합니다.

> **`ssamd`가 실행 중이며 접근 가능해야 합니다.** `ssam`은 클라이언트일 뿐입니다.
> 데몬 실행은 [SSAM 실행하기](../guides/serving-ssam.ko.md)를 참고하세요.

---

## Table of Contents

1. [Connecting to the daemon](#connecting-to-the-daemon)
2. [Common workflows](#common-workflows)
3. [Commands](#commands)
   - [`list`](#list)
   - [`start`](#start)
   - [`stop`](#stop)
   - [`install`](#install)
   - [`remove`](#remove)
   - [`wipe-data`](#wipe-data)
   - [`pkg-info`](#pkg-info)
   - [`timeline-info`](#timeline-info)

---

## Connecting to the daemon

`ssam`은 고정된 Linux abstract Unix domain socket `@ssamd`를 통해 `ssamd`에 *gRPC*로 연결합니다:

```
unix://\0ssamd
```

| 설정 | 값 |
| --- | --- | --- |
| 엔드포인트 | `unix://\0ssamd` |
| 전송 | abstract Unix domain socket 기반 로컬 gRPC |
| 인증 | `SO_PEERCRED` 및 `SO_PEERGROUPS`를 통한 peer effective 또는 supplementary GID 중 하나가 데몬 effective GID 또는 `[rpc].allowed_gids`에 포함된 GID와 일치 |

peer의 effective 또는 supplementary GID 중 하나가 데몬의 effective GID와 일치하면 항상 허용됩니다.
추가 peer effective 또는 supplementary GID는 `[rpc].allowed_gids`로 설정할 수 있으며 기본값은 빈 목록입니다.
인증에는 UID를 사용하지 않습니다.

제어 API의 세부는 [Remocon gRPC 제어 API](remocon-rpc.ko.md)를 참고하세요.

---

## Common workflows

패키지를 설치하고, 실행하고, 확인하고, 제거하기:

```console
$ ssam install ./helloworld-1.0.0.ssam   # SSAM 패키지 파일에서 검증 + 설치
$ ssam start helloworld                   # 컨테이너 시작
$ ssam list                               # Running 상태 확인
$ ssam stop helloworld                    # 중지
$ ssam remove helloworld                  # 제거
```

컨테이너 stdout/stderr와 생명주기는 패키지의 *systemd* unit 아래 `journalctl`에 나타납니다([SSAM 실행하기 → 검증](../guides/serving-ssam.ko.md#step-6--verify) 참조).

---

## Commands

일반 형식:

```
ssam <COMMAND> [ARGS...]
```

### 전역 옵션

| 옵션 | 설명 |
| --- | --- |
| `-V`, `--version` | `ssamd`에 연결하지 않고 워크스페이스에서 관리하는 `ssam` 패키지의 버전을 출력합니다. |

### `list`

모든 패키지와 그 상태를 나열합니다.

```
ssam list [--detail]
```

| 옵션 | 설명 |
| --- | --- |
| `--detail` | 각 패키지의 상세 정보 표시(경로, 버전, 설명, 쿼터). |

```console
$ ssam list
List of packages
Package name         Status
===============================================
helloworld           Ready
```

상태 값은 데몬의 [패키지 상태](../architecture/execution.ko.md#package-state-machine)(`Ready`, `Running`, `SettingUp`, `Broken`, …)에 대응합니다.

### `start`

설치된 하나 이상의 패키지를 시작합니다.

```
ssam start <PACKAGE_NAMES>...
ssam start --all
```

| 인자 / 옵션 | 설명 |
| --- | --- |
| `<PACKAGE_NAMES>...` | 시작할 하나 이상의 패키지 이름. |
| `--all` | **모든** 설치된 패키지를 시작. 지정 패키지와 상호 배타적. |

패키지 목록 **또는** `--all` 중 하나가 필요합니다 — 인자 없는 `ssam start`는 거부되므로, 실수로 전체 패키지에 명령이 적용되는 일을 막습니다.
패키지별 결과는 `[OK] <name>` 또는 `[FAIL] <name>: <message>`로 출력됩니다.

```console
$ ssam start helloworld web-app
  [OK] helloworld
  [OK] web-app
```

### `stop`

실행 중인 하나 이상의 패키지를 중지합니다.
`start`와 동일한 인자 형태입니다.

```
ssam stop <PACKAGE_NAMES>...
ssam stop --all
```

| 인자 / 옵션 | 설명 |
| --- | --- |
| `<PACKAGE_NAMES>...` | 중지할 하나 이상의 패키지 이름. |
| `--all` | **모든** 설치된 패키지를 중지. 지정 패키지와 상호 배타적. |

### `install`

로컬 SSAM 패키지 파일에서 패키지를 설치합니다.
데몬은 설치 전에 패키지의 서명과 무결성을 검증합니다.

```
ssam install <PACKAGE_PATH> [-d|--downgrade] [-r|--remove_data]
```

| 인자 / 옵션 | 설명 |
| --- | --- |
| `<PACKAGE_PATH>` | SSAM 패키지 파일 경로. |
| `-d`, `--downgrade` | 이미 설치된 것보다 **낮은 버전** 이어도 설치 허용. |
| `-r`, `--remove_data` | (재)설치 시 패키지에 연결된 모든 데이터 제거. |

`<PACKAGE_PATH>`가 상대 경로면 클라이언트와 데몬이 같은 로컬 전송을 사용하므로 로컬 파일시스템 기준으로 해석됩니다.

```console
$ ssam install ./helloworld-1.0.0.ssam
```

### `remove`

설치된 패키지를 이름으로 제거(삭제)합니다.

```
ssam remove <PACKAGE_NAME>
```

| 인자 | 설명 |
| --- | --- |
| `<PACKAGE_NAME>` | 제거할 설치된 패키지 이름. |

```console
$ ssam remove helloworld
```

### `wipe-data`

설치된 하나 이상의 패키지의 데이터 볼륨을 지우되, 설치 상태는 유지합니다.
setup을 마치고 정지된(`Ready`) 패키지나 정리된(`Cleaned`) 패키지만 지울 수 있으며, setup 전의 `Verified`를 포함한 다른 상태에서는 거부됩니다.
`start`/`stop`과 동일한 인자 형태입니다.

```
ssam wipe-data <PACKAGE_NAMES>...
ssam wipe-data --all
```

| 인자 / 옵션 | 설명 |
| --- | --- |
| `<PACKAGE_NAMES>...` | 데이터를 지울 하나 이상의 패키지 이름. |
| `--all` | **모든** 설치된 패키지의 데이터를 지움. 지정 패키지와 상호 배타적. |

```console
$ ssam wipe-data helloworld
```

### `pkg-info`

URI로 참조되는 하나 이상의 패키지 정보를 표시합니다.

```
ssam pkg-info <PACKAGE_URI>...
```

| URI 형식 | 의미 |
| --- | --- |
| `ssam://<name>` | 이름으로 참조되는 **설치된** 패키지. |
| `file:///<absolute-path>` | 디스크의 SSAM 패키지 **파일**(설치 여부 무관). 경로는 절대 경로여야 합니다. |

```console
$ ssam pkg-info ssam://helloworld
$ ssam pkg-info file:///home/user/helloworld-1.0.0.ssam
```

### `timeline-info`

데몬의 타임라인 / 가동 시간 정보를 표시합니다.

```
ssam timeline-info
```

---

> 📖 이 명령어들이 대응하는 하위 요청/응답 계약은 [Remocon gRPC 제어 API 레퍼런스](remocon-rpc.ko.md)를 참고하세요.
