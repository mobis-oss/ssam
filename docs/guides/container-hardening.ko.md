# SSAM Container Hardening

<p>
  <a href="container-hardening.md"><img src="https://img.shields.io/badge/lang-English-blue" alt="English"></a>
</p>

실행 중인 SSAM 컨테이너가 **커널과 호스트 시스템에 대해 할 수 있는 일** 을 제한하는 두 가지를 다룹니다.
이 문서는 각각을 운영자와 패키지 작성자가 **어떻게 설정하는지** 설명합니다:

- **시스템 콜 필터(seccomp)** — 컨테이너가 호출할 수 있는 시스템 콜을 제한합니다.
- **강제 접근 제어(AppArmor)** — 컨테이너가 접근할 수 있는 대상(파일·권한·네트워크)을 제한합니다.

> 📖 패키지 무결성(서명·dm-verity·읽기 전용 파일시스템)은 [Package Integrity](package-integrity.ko.md), 네트워크·자원 격리는 [Container Isolation](container-isolation.ko.md) 가이드를 참고하세요.

> 📖 seccomp·MAC이 어떻게 *OCI Runtime* 에 위임·집행되는지(설계·모델)는 [Container Isolation 설계 문서](../architecture/container-isolation.ko.md)를 참고하세요.

---

## Table of Contents

1. [System-call filtering — seccomp](#system-call-filtering--seccomp)
2. [Mandatory access control — AppArmor](#mandatory-access-control--apparmor)

---

## System-call filtering — seccomp

*seccomp* 은 컨테이너가 호출할 수 있는 Linux **시스템 콜** 을 제한해 커널을 노리는 공격 경로를 좁힙니다.
SSAM은 이를 [`container.security.seccomp`](../reference/package-config.ko.md#seccomp) 설정과 패키지에 담기는 `seccomp.json` 정책 파일로 제어합니다.

정책 파일은 **서명 메타데이터의 일부** 로 패키지에 포함되므로, 서명 이후에는 조작할 수 없습니다.

**설정 절차.**

1. **템플릿 생성.** `ssam-wrap --prepare <이름>`이 작업 디렉터리에 `seccomp.json`을 생성합니다.
   이 템플릿은 **Docker의 기본 seccomp 프로파일** 로, [OCI seccomp 설정](https://github.com/opencontainers/runtime-spec/blob/main/config-linux.md#seccomp)과 공유하는 표준 `SCMP_ACT_*` 액션 · `archMap` · `syscalls` 스키마를 씁니다.
   기존 `seccomp.json`이 있으면 덮어쓰지 않습니다.
2. **필요하면 편집.** 대부분의 패키지는 템플릿을 그대로 써도 됩니다.
   특정 시스템 콜을 허용하거나 거부하려면 `syscalls` 항목을 편집하거나 직접 정책을 작성합니다.
3. **활성화.** `config.toml`에서 `seccomp = true`(권장)로 둡니다.
   `false`는 필터를 적용하지 않으니 디버깅 용도로만 추천합니다.
4. **wrap.** `ssam-wrap`이 `seccomp.json`을 패키지에 담고 서명합니다.

**예를 들어,**

컨테이너 내부에서 실행되는 앱이 파일 권한을 바꾸지 못하게 `chmod`를 막으려면, `seccomp.json` 템플릿의 허용 목록(`SCMP_ACT_ALLOW`)에서 그 이름을 빼면 됩니다.
기본 동작이 `SCMP_ACT_ERRNO`(거부) 라 허용 목록에 없는 시스템 콜은 자동으로 막힙니다:

```diff
 {
-  "names": ["chmod", "fchmod", "fchmodat"],
+  "names": ["fchmod", "fchmodat"],
   "action": "SCMP_ACT_ALLOW"
 }
```

이제 컨테이너의 `chmod(2)` 호출은 거부되고, 호출한 프로세스는 오류(`ENOSYS`)를 받습니다.

실행 시점에 `ssamd`가 이 정책을 컨테이너의 *OCI Runtime config* 에 주입합니다.
정책에 매핑할 수 없는 항목이 있으면 필터 없이 실행하는 대신 **패키지 기동에 실패합니다**.

---

## Mandatory access control — AppArmor

강제 접근 제어(MAC)는 컨테이너 프로세스가 접근할 수 있는 대상(파일·권한·네트워크 등)을 정책으로 제한합니다.
SSAM은 *AppArmor* 프로파일로 이를 적용하며, [`container.security.mac`](../reference/package-config.ko.md#mac)으로 제어합니다.

**SSAM은 AppArmor 프로파일을 이름으로 *선택* 합니다.** 프로파일을 작성해 커널에 로드하는 것은 **호스트 시스템의 책임** 입니다.
그러므로 호스트 시스템은 이미지 빌드나 디바이스 프로비저닝의 일부로서 프로파일을 사전에 로드하도록 구성하여야 합니다.

> 📖 `[container.security]`의 설정 필드는 [패키지 설정 레퍼런스](../reference/package-config.ko.md#containersecurity)를 참고하세요.

`mac = true` 일 때 `ssamd`는:

1. **`ssam-prof-<패키지명>`** 이름의 프로파일을 찾습니다(패키지 `config.toml`의 `name`).
2. 로드돼 있으면 컨테이너에 적용합니다.
3. 로드돼 있지 **않으면** **`container-default`** 프로파일로 폴백합니다.
4. 호스트 시스템에 *AppArmor* 지원이 없으면 — `mac = true` 여도 — 격리를 건너뜁니다.
5. 프로파일 조회 자체가 실패하면(예: 로드된 프로파일 목록을 읽다가 I/O 에러) 마찬가지로 건너뜁니다 — `container-default`는 조회가 성공했는데 지정한 이름의 프로파일만 없을 때만 쓰이고, 조회 실패 시에는 쓰이지 않습니다.

**설정 절차.**

1. `config.toml`에서 `mac = true`로 둡니다.
2. **`ssam-prof-<패키지명>`** 프로파일을 작성합니다(`<패키지명>`은 `config.toml`의 [`name`](../reference/package-config.ko.md#name)).
   `helloworld` 일 때의 골격 예시입니다:

   ```
   #include <tunables/global>

   profile ssam-prof-helloworld flags=(attach_disconnected) {
     #include <abstractions/base>

     # 컨테이너가 접근할 경로만 허용
     /app/** r,
     /tmp/** rw,
   }
   ```

3. 대상 디바이스의 커널에 **미리 로드** 합니다(SSAM 외부에서):

   ```console
   # apparmor_parser -r /etc/apparmor.d/ssam-prof-helloworld
   ```

4. 전용 프로파일 없이 배포되는 패키지를 위해 `container-default` 프로파일을 폴백으로 제공해 둘 수 있습니다.

> ⚠️ `runtime.json`의 `apparmorProfile` 필드에 이름을 직접 적어도, 실행 시 `ssamd`가 패키지명에서 유도한 이름으로 *OCI Runtime config* 의 그 필드를 **덮어써 버리므로 무시됩니다.** MAC을 완전히 끄려면 `mac = false`로 설정하세요.
