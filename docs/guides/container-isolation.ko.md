# SSAM Container Isolation

<p>
  <a href="container-isolation.md"><img src="https://img.shields.io/badge/lang-English-blue" alt="English"></a>
</p>

SSAM으로 컨테이너를 실행할 때 컨테이너의 **네트워크** 와 **자원(cgroup·저장 쿼터)** 을 설정할 수 있습니다.
이 가이드는 그 설정 방법을 다룹니다.

> 📖 실행 중 컨테이너의 행위 제한(seccomp·AppArmor)은 [Container Hardening](container-hardening.ko.md), 패키지 무결성(서명·dm-verity·읽기 전용 파일시스템)은 [Package Integrity](package-integrity.ko.md) 가이드를 참고하세요.

> 📖 격리가 어떻게 *OCI Runtime* 에 위임·집행되는지(설계·모델)는 [Container Isolation 설계 문서](../architecture/container-isolation.ko.md)를 참고하세요.

---

## Table of Contents

1. [Networking](#networking)
   - [Host networking](#host-networking)
   - [No networking](#no-networking)
   - [Bridge networking](#bridge-networking)
   - [Using predefined networking](#using-predefined-networking)
2. [Container cgroup](#container-cgroup-packages_cgroup)
3. [Storage quota](#storage-quota-storage_limit)

---

## Networking

네트워크 모드는 패키지별로 [`[container.network]`](../reference/package-config.ko.md#containernetwork)의 `mode`로 선택합니다.

| 모드 | 컨테이너가 받는 것 |
| --- | --- |
| **`host`** | 호스트 네트워크 namespace 공유 — 격리 없음. |
| **`none`** | 네트워크 없음. |
| **`bridge`** | 내부 브리지에 연결 — 같은 브리지끼리 통신. **데몬 브리지 설정 필수.** |
| **절대 경로** | `ssamd` 외부에서 관리하는 특정 네트워크 namespace에 연결. |

### Host networking

`config.toml`에서 `mode`를 `host`로 지정합니다(또는 `[container.network]`를 생략):

```toml
[container.network]
mode = "host"
```

- 컨테이너가 호스트의 네트워크 namespace를 그대로 공유합니다.
- 격리가 없어 오버헤드가 가장 작고, `[container.network]`를 생략하면 이 모드가 기본값입니다.
- `ssamd`는 별도 설정이 필요 없습니다.

### No networking

`config.toml`에서 `mode`를 `none`으로 지정합니다:

```toml
[container.network]
mode = "none"
```

- 컨테이너가 격리된 빈 네트워크 namespace를 받습니다.
- 외부는 물론 다른 컨테이너와도 네트워크 연결이 없습니다.
- `ssamd`는 별도 설정이 필요 없습니다.

### Bridge networking

#### 개요

`bridge` 모드는 컨테이너를 `ssamd`가 관리하는 **내부** 브리지에 배치합니다:

- **격리.** 게이트웨이·NAT·외부 라우팅이 없습니다 — 브리지에 연결된 컨테이너는 같은 브리지의 다른 컨테이너에는 닿지만 외부 네트워크에는 닿지 않습니다.
- **공유 네트워크.** 같은 [`network_name`](../reference/package-config.ko.md#containernetworkbridge)을 설정한 패키지들은 같은 브리지에 연결되어 서로 통신할 수 있습니다.
  생략하면 패키지 전용 브리지를 갖습니다.
- **주소 할당.** `ssamd`가 데몬의 브리지 주소 풀에서 각 컨테이너에 주소를 할당하고, 브리지 인터페이스 이름을 자동으로 지정합니다.
- **데몬 브리지 설정 필요.** 데몬 브리지 설정([`[network.bridge]`](../reference/daemon-config.ko.md#networkbridge))이 비활성인데 패키지가 `bridge`를 요청하면 `ssamd`는 패키지를 **거부** 합니다.

브리지 모드는 **데몬** 과 **패키지** 양쪽 설정이 맞아야 동작합니다.

#### ssamd 설정

`ssamd.toml`에서 관리 브리지를 활성화합니다:

```toml
[network.bridge]
enabled = true

[network.bridge.addr_pool]      # 선택
base = "172.20.0.0/16"          # 주소 풀 (기본: 172.20.0.0/16)
size = 29                       # 브리지별 서브넷 prefix (기본: 29)
```

브리지 모드는 호스트의 `bridge` / `veth` 커널 기능도 필요합니다.

> 📖 전체 키는 [`[network]` 레퍼런스](../reference/daemon-config.ko.md#network), 디바이스에서 브리지를 활성화·적용하는 운영 절차는 [SSAM 실행하기 → Networking](serving-ssam.ko.md#networking)을 참고하세요.

#### 개별 패키지 설정

`config.toml`에서 모드와 선택적 브리지 설정을 선언합니다:

```toml
[container.network]
mode = "bridge"

[container.network.bridge]      # 선택
network_name   = "shared-net-a" # 선택 (기본: 전용 브리지) — 패키지 간 브리지 공유
interface_name = "eth0"         # 선택 (기본: eth0) — 컨테이너 측 인터페이스 이름
```

> 📖 전체 키는 [`[container.network]` 레퍼런스](../reference/package-config.ko.md#containernetwork)를 참고하세요.

### Using predefined networking

SSAM은 호스트 시스템에 미리 정의해 둔 네트워크를 패키지가 사용하도록 할 수도 있습니다.
예를 들어, 다음과 같이 호스트 시스템에서 컨테이너를 위한 네트워크를 설정한다면:

```console
# ip netns add my-netns
# ip netns exec my-netns ip link set lo up
```

패키지 생성 시 `config.toml`의 `mode`에 그 경로를 지정합니다:

```toml
[container.network]
mode = "/var/run/netns/my-netns"
```

- 컨테이너는 실행 시 설정된 namespace를 사용하며, 미리 생성되어 있지 않으면 실행에 실패합니다.
- 설정된 namespace를 준비하는 주체는 호스트 시스템이므로, 특정 설정이 존재하는 시스템에 의존하는 패키지가 된다는 점에 주의가 필요합니다.
- `ssamd`는 별도 설정이 필요 없습니다.

---

## Container cgroup (`packages_cgroup`)

`ssamd.toml`의 [`packages_cgroup`](../reference/daemon-config.ko.md#packages_cgroup)은 모든 패키지 컨테이너가 놓일 **root cgroup** 을 지정합니다.

- **설정하지 않으면(기본설정)** `ssamd`는 root cgroup으로 호스트의 `system.slice`를 사용합니다.
- **값을 지정하면** 그 경로를 root cgroup으로 사용합니다.

root cgroup이 정해지면 `ssamd`는 각 컨테이너 *OCI Runtime config*의 `cgroupsPath`를 `<packages_cgroup>/<패키지명>.service/container`로 지정합니다.
이를 통해 모든 패키지 컨테이너가 함께 사용하는 자원의 상한을 제한할 수 있습니다.
지정한 root cgroup은 **`ssamd` 시작 전에 존재해야** 하며, `ssamd`는 이를 생성하지 않습니다(없으면 데몬이 중단됩니다).

값을 지정하는 방법은 두 가지입니다.
어느 쪽이든 root cgroup은 호스트 시스템이 미리 준비하고, `ssamd.toml`은 그 이름을 가리키도록 설정합니다.

### systemd slice unit 활용하기

root cgroup으로 *systemd* **slice unit** 을 사용할 수 있습니다.
예를 들어, root cgroup으로 쓸 slice를 `ssam.slice`로 한 경우:
- `ssamd.toml`에 `packages_cgroup = "ssam.slice"`로 지정
- `ssamd`는 각 컨테이너별 *systemd* service unit(`<패키지명>.service`)의 `Slice=`를 `ssam.slice`로 설정
- `ssamd`는 각 컨테이너별 *OCI Runtime config*의 `cgroupsPath`를 `ssam.slice/<패키지명>.service/container`로 설정

이 slice는 호스트 시스템에 미리 준비해 둡니다 — 예를 들어 다음과 같이:

```console
# cat > /etc/systemd/system/ssam.slice <<'EOF'
[Unit]
Description=SSAM package containers
EOF
# systemctl daemon-reload
# systemctl start ssam.slice
```

> 📖 slice unit 작성과 slice 단위 자원 제한(`CPUQuota=`·`MemoryMax=` 등)은 `systemd.slice(5)`·`systemd.resource-control(5)` man 페이지를 참고하세요.

### cgroup 사용하기

root cgroup으로 cgroup을 직접 만들어 사용할 수 있습니다.
예를 들어, root cgroup으로 쓸 이름을 `ssam`으로 한 경우:
- `ssamd.toml`에 `packages_cgroup = "ssam"`으로 지정
- `ssamd`는 각 컨테이너별 *OCI Runtime config*의 `cgroupsPath`를 `ssam/<패키지명>.service/container`로 설정
- `ssamd`는 이 값이 *systemd* slice가 아니므로 각 컨테이너별 *systemd* service unit에 `Slice=`를 설정하지 않고 경고 로그를 출력

이 cgroup은 호스트 시스템에 미리 만들어 둡니다 — 예를 들어 다음과 같이:

```console
# mkdir -p /sys/fs/cgroup/ssam
```

> 📖 이 cgroup에 CPU·메모리 등 **자원 제한** 을 설정하는 방법은 커널 [cgroup v2 문서](https://docs.kernel.org/admin-guide/cgroup-v2.html)나 `cgroups(7)` man 페이지를 참고하세요.

---

## Storage quota (`storage_limit`)

패키지가 [`container.storage_limit`](../reference/package-config.ko.md#storage_limit)을 선언하면 `ssamd`는 그 패키지의 데이터 디렉터리에 용량 상한을 적용합니다.
`ssamd`는 이를 *ext4* **project quota** 로 구현합니다.
이를 사용하려면 **호스트 시스템의 커널 설정**과 **[`packages_data_root`](../reference/daemon-config.ko.md#packages_data_root)로 설정한 파일시스템**이 각각 다음을 갖춰야 합니다.

**커널 설정**

- `CONFIG_EXT4_FS` — *ext4* 파일시스템
- `CONFIG_QUOTA` — 디스크 쿼터 지원
- `CONFIG_QFMT_V2` — project quota 지원(vfsv1 쿼터 포맷)

**파일시스템**

- **ext4** 파일시스템일 것.
- **project quota가 활성** 화되어 있을 것 — 예를 들어 `mkfs.ext4 -O quota,project` 로 포맷(또는 `tune2fs`로 기능 추가)하고 project quota로 마운트(`-o prjquota`).

전제가 충족되지 않으면(*ext4* 가 아니거나 project quota 미활성) `ssamd`는 경고를 남기고 `storage_limit`을 적용하지 않은 채 계속 실행하며, 컨테이너는 용량 제한 없이 기동합니다.
상한을 실제로 강제하려면 위 조건을 미리 갖추세요.
