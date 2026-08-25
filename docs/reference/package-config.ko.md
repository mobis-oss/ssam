# 패키지 설정(`config.toml`) 레퍼런스

<p>
  <a href="package-config.md"><img src="https://img.shields.io/badge/lang-English-blue" alt="English"></a>
</p>

`config.toml` 은 SSAM 패키지의 **패키지 설정** 파일입니다. 패키지의 정체성
(이름, 버전), 데이터/쿼터 및 네트워크 설정, 보안 태세, 그리고 `ssamd` 가
패키지의 컨테이너를 실행할 때 사용하는 서비스 모델을 담습니다.

> `config.toml` 을 채우는 단계별 안내는 **[SSAM 만들기 — 패키지 생성 가이드](../guides/wrapping-a-ssam.ko.md)** 를
> 참고하세요. 이 문서는 설정 키 **레퍼런스** 입니다.

`ssam-wrap` 은 [prepare 모드](ssam-wrap.ko.md#command-line-reference)(`--prepare <NAME>`)
에서 템플릿 `config.toml` 을 생성합니다.

> **값 검증 시점.** `ssam-wrap` 은 패키지 **version**(유효한
> [semver](https://semver.org/)여야 함)을 wrap 시점에 검증합니다. 그 외
> 대부분의 필드 — 네트워크 `mode`, `service_type` — 는
> 패키지가 로드되거나 시작될 때 `ssamd` 가 읽어들여 검증합니다. `ssam-wrap` 을
> 통과한 잘못된 값은 대상 시스템에서 로드/실행 오류로 나타납니다.

---

## Table of Contents

1. [File structure at a glance](#file-structure-at-a-glance)
2. [`[package]`](#package)
3. [`[container]`](#container)
4. [`[container.security]`](#containersecurity)
5. [`[container.network]`](#containernetwork)
6. [`[container.network.bridge]`](#containernetworkbridge)
7. [`[service]`](#service)
8. [Required vs. optional keys](#required-vs-optional-keys)
9. [Complete examples](#complete-examples)

---

## File structure at a glance

```toml
[package]
name = "package_name"
description = "A short description of the package"
version = "0.0.1"
autostart = true                # 선택

[container]
storage_limit = 1000            # 선택, 킬로바이트
data_dirs = "/path1:/path2"     # 선택

[container.security]
seccomp = true
mac = true

[container.network]             # 선택 테이블
mode = "host"                   # 선택

[container.network.bridge]      # 선택, mode = "bridge"일 때만
network_name = "shared-net-a"   # 선택
interface_name = "eth0"         # 선택

[service]
service_type = "simple"
bus_name = "my.bus.name"        # 선택
remain_after_exit = false       # 선택
```

`[package]`, `[container]`, `[container.security]`, `[service]` 테이블은
**필수** 입니다. `[container.network]` 테이블(및 그 `bridge` 하위 테이블)은
**선택** 입니다.

---

## `[package]`

패키지의 정체성과 생명주기.

### `name`

- **타입:** string · **필수**
- 패키지 이름. `ssamd` 가 패키지를 설치·목록화·실행할 때 사용하는 식별자입니다
  (systemd 유닛 이름이 됩니다). 따라서 고유하고 파일시스템 친화적이어야 합니다.
- prepare 모드에서 `ssam-wrap` 은 `--prepare` 에 전달한 `<NAME>` 으로 이 값을
  미리 채웁니다.

### `description`

- **타입:** string · **필수**
- 사람이 읽을 수 있는 설명. 패키지 조회 gRPC 응답에 실리거나, 대상 시스템에서 systemd 서비스 설명으로 표시됩니다.

### `version`

- **타입:** string · **필수**
- 패키지 버전. **유효한 [semver](https://semver.org/)여야 합니다**
  (예: `1.0.0`, `0.2.1-rc.1`). 그렇지 않으면 `ssam-wrap` 이 오류와 함께 빌드를 거부합니다.
- 업그레이드 비교와 기본 출력 파일명 `<name>-<version>.ssam` 에 사용됩니다.

### `autostart`

- **타입:** boolean · **선택** · **기본값:** `true`
- `ssamd` 가 패키지 로드 시(예: 부팅 시) 자동으로 시작할지 여부. gRPC 클라이언트로 시작
  시점을 따로 관리하려는 패키지는 `false` 로 설정하세요.

---

## `[container]`

실행 중인 컨테이너의 데이터 영역 및 스토리지 설정.

### `storage_limit`

- **타입:** integer(킬로바이트) · **선택**
- 패키지의 쓰기 가능한 데이터 영역에 *ext4* **프로젝트 쿼터** 로 적용되는
  **킬로바이트** 단위 쿼터. 쿼터가 없으면 생략합니다.
- 강제 적용에는 대상 커널의 *ext4* 프로젝트 쿼터 지원이 필요합니다(`ssamd`
  요구 사항에서 선택 항목으로 표시됨). 지원이 없으면 제한이 적용되지 않습니다.

### `data_dirs`

- **타입:** string(콜론으로 구분된 경로) · **선택**
- 컨테이너 안에서 **읽기·쓰기가 필요한 디렉터리**(컨테이너 절대 경로)들을 `:` 로 구분해
  나열합니다. 패키지 파일시스템 자체는 읽기 전용이므로, 앱에서 쓰기가 되어야 하는 경로는
  여기에 선언해야 합니다.
- `ssamd` 는 [`packages_data_root`](daemon-config.ko.md#packages_data_root) 아래에
  이 경로들을 영속 데이터 디렉터리로 만들어 컨테이너에 bind 마운트하고, 용량은
  [`storage_limit`](#storage_limit) 으로 제한합니다. 확보·마운트의 상세 동작은
  [Execution](../architecture/execution.ko.md#the-execution-pipeline)을 참고하세요.

```toml
data_dirs = "/app/data:/app/logs"
```

---

## `[container.security]`

보안 강화 토글.

### `seccomp`

- **타입:** boolean · **필수** · **권장:** `true`
- `true` 로 설정하면 컨테이너가 패키지 내에 포함된 *seccomp* 정책 하에서 실행됩니다.
- 정책은 `seccomp.json` 파일로 패키지에 포함됩니다. `ssam-wrap --prepare` 에서 제공하는
  템플릿 `seccomp.json` 을 편집하거나, 완전히 새로 작성도 가능합니다.
- 디버깅이나 워크로드가 실제로 필터링되지 않은 syscall이 필요한 경우에만 비활성화하세요.

### `mac`

- **타입:** boolean · **필수** · **권장:** `true`
- 컨테이너에 대한 강제 접근 제어(MAC) 격리를 활성화합니다. MAC은 *AppArmor* 로 제공되며,
  대상 시스템에 **미리 약속된 이름의 AppArmor 프로파일** 이 로드되어 있어야 합니다.
- 프로파일 이름 규칙과 작성·로드 방법 등 상세는 [Container Isolation](../architecture/container-isolation.ko.md)
  과 [컨테이너 보안 가이드](../guides/securing-containers.ko.md)를 참고하세요.

---

## `[container.network]`

컨테이너 네트워킹. **테이블 전체가 선택** 입니다 — 생략하면 기본값(`host`)
네트워킹을 사용합니다.

### `mode`

- **타입:** string · **선택** · **기본값:** `host`
- 다음 중 하나:
  - **`host`** — 컨테이너가 호스트 네트워크 namespace를 공유합니다. 격리 없음,
    최소 오버헤드. `mode` 설정이 없을 때의 기본값입니다.
  - **`none`** — 네트워크 연결 없음. 컨테이너는 격리된 빈 네트워크 namespace를
    받습니다.
  - **`bridge`** — 컨테이너가 내부 브리지에 연결된 자체 네트워크 namespace를
    받습니다(게이트웨이·NAT·외부 라우팅 없음). 같은 브리지에 연결된 컨테이너끼리는
    통신이 가능하지만 외부 네트워크에는 닿지 않습니다.
    [`[container.network.bridge]`](#containernetworkbridge) 참조.
  - **절대 경로** — 외부에서 별도로 설정·관리하는 네트워크 namespace의 경로. 컨테이너 실행
    시 해당 namespace를 사용하며, 경로에 namespace가 없으면 실행에 실패합니다.

절대 경로도 아니고 `host`/`none`/`bridge` 중 하나도 아닌 값은 거부됩니다.

> **Bridge는 데몬 브리지 설정이 필요합니다:** 패키지가 `bridge` 를 요청했지만 데몬의
> 브리지 네트워크 설정이 비활성화되어 있으면, `ssamd` 는 패키지를 **거부** 합니다.

---

## `[container.network.bridge]`

`mode = "bridge"` 일 때만 의미가 있습니다. 두 키 모두 선택입니다.

### `network_name`

- **타입:** string · **선택**
- 공유 브리지의 이름. 같은 `network_name` 을 설정한 패키지들은 같은 내부 브리지에
  연결되어 서로 통신할 수 있습니다. 생략하면 패키지 전용 브리지를 갖습니다.

### `interface_name`

- **타입:** string · **선택**
- 컨테이너 내부 네트워크 인터페이스의 이름. 특정 인터페이스 이름을 기대하는 앱을 위한
  옵션입니다. 생략하면 `eth0` 으로 설정됩니다.

---

## `[service]`

`ssamd` 가 컨테이너를 systemd 관리 서비스로 실행하는 방식.

### `service_type`

- **타입:** string · **필수**
- systemd 서비스 모델. 유닛의 `Type=` 에 직접 매핑됩니다. 다음 중 하나:

| 값 | systemd `Type=` | 비고 |
| --- | --- | --- |
| `simple` | `simple` | 장시간 실행 프로세스. 즉시 시작된 것으로 간주. |
| `exec` | `exec` | `simple` 과 유사하나, 바이너리가 실행된 후에만 시작된 것으로 간주. |
| `forking` | `forking` | 프로세스가 fork하고 부모가 종료. |
| `oneshot` | `oneshot` | 한 번 완료까지 실행. 흔히 `remain_after_exit` 와 함께 사용. |
| `dbus` | `dbus` | D-Bus 시스템 버스에서 `bus_name` 을 획득하면 준비 완료. `bus_name` 과 함께 써야 함(현재 강제되지는 않음). |
| `notify` | `notify` | `sd_notify` 를 보내면 준비 완료. `ssamd` 가 `NotifyAccess=all` 설정. `runtime.json` 에 `/run` 마운트도 필요. |
| `idle` | `idle` | `simple` 과 유사하나, 다른 작업이 디스패치될 때까지 실행이 지연됨. |

> 각 systemd 서비스 타입의 자세한 동작은 man page *systemd.service(5)* 를 참고하세요.

> **`notify` 서비스는 `/run` 이 필요합니다.** `service_type = "notify"` 이면
> `runtime.json` 에 `/run` 마운트가 선언되어 있어야 하며, 없으면 `ssam-wrap` 이
> 오류로 wrap을 중단합니다.
> [런타임 설정](../guides/wrapping-a-ssam.ko.md#step-4--configure-the-runtime-runtimejson)을 참고하세요.

### `bus_name`

- **타입:** string · **선택**
- 서비스가 획득하는 *D-Bus* 이름. 유닛의 `BusName=` 을 채우는 데 사용됩니다.
  `service_type = "dbus"` 일 때 설정해야 하지만, 이를 검증하는 코드는 없어서
  `bus_name` 이 빠져도 거부되지 않고 `BusName=` 없는 `dbus` 타입 유닛이 그냥
  조용히 만들어집니다. 다른 서비스 타입에서는 무시됩니다.

```toml
bus_name = "com.example.MyService"
```

### `remain_after_exit`

- **타입:** boolean · **선택** · **기본값:** `false`
- systemd `RemainAfterExit=` 에 매핑됩니다. `true` 이면 메인 프로세스가 종료된
  후에도 서비스가 활성 상태로 간주됩니다 — 흔히 `service_type = "oneshot"` 과
  함께 사용됩니다.

---

## Required vs. optional keys

| 키 | 필수 여부 | 기본값 |
| --- | --- | --- |
| `package.name` | ✅ 필수 | — |
| `package.description` | ✅ 필수 | — |
| `package.version` | ✅ 필수 (유효한 semver) | — |
| `package.autostart` | 선택 | `true` |
| `container.storage_limit` | 선택 | 쿼터 없음 |
| `container.data_dirs` | 선택 | 없음 |
| `container.security.seccomp` | ✅ 필수 | (권장 `true`) |
| `container.security.mac` | ✅ 필수 | (권장 `true`) |
| `container.network` (테이블) | 선택 | host 네트워킹 |
| `container.network.mode` | 선택 | `host` |
| `container.network.bridge.network_name` | 선택 | 전용 브리지 |
| `container.network.bridge.interface_name` | 선택 | `eth0` |
| `service.service_type` | ✅ 필수 | — |
| `service.bus_name` | 선택 (`dbus` 에는 권장, 강제는 아님) | 없음 |
| `service.remain_after_exit` | 선택 | `false` |

---

## Complete examples

### Minimal: a simple host-networked service

```toml
[package]
name = "hello"
description = "Hello world service"
version = "1.0.0"

[container]

[container.security]
seccomp = true
mac = true

[service]
service_type = "simple"
```

### Bridge networking with a shared network and a data area

```toml
[package]
name = "web-app"
description = "Web application with persistent storage"
version = "2.3.1"
autostart = true

[container]
storage_limit = 50000            # 50 MB 데이터 영역 쿼터 (프로젝트 쿼터)
data_dirs = "/app/data:/app/logs"

[container.security]
seccomp = true
mac = true

[container.network]
mode = "bridge"

[container.network.bridge]
network_name = "shared-net-a"
interface_name = "eth0"

[service]
service_type = "notify"          # runtime.json에 /run 마운트 필요
remain_after_exit = false
```

### A D-Bus service

```toml
[package]
name = "dbus-daemon-app"
description = "A D-Bus activated service"
version = "0.1.0"

[container]

[container.security]
seccomp = true
mac = true

[service]
service_type = "dbus"
bus_name = "com.example.MyService"
```

---

전체 패키징 작업 흐름은 [ssam-wrap 명령어 레퍼런스](ssam-wrap.ko.md)를 참고하세요.
