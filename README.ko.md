<p align="center">
  <img src="docs/assets/ssam-logo.png" alt="SSAM Logo" width="256">
</p>

# SSAM
* [쌈](https://en.wikipedia.org/wiki/Ssam)
* SSAM은 Automotive 등과 같은 시스템 리소스가 한정적인 Embedded 환경에서 빠른 Container-native app의 실행과 실행 시점의 지속적인 무결성 보장을 지원하기 위해 개발한 솔루션입니다.
* SSAM은 OCI Runtime 표준을 활용하여 Container-native app을 실행합니다.
* SSAM은 Container-native app의 실행환경 구성을 위해 Filesystem image를 활용하는 패키지를 정의합니다.
* SSAM은 embedded Linux 환경을 위해 단순화된 컨테이너 패키지의 정의 및 관리, Container의 실행을 관장합니다.
* SSAM은 `dm-verity`를 통해 Container의 실행 중 무결성 보장을 지원합니다.

## Background
*SDV*(Software-Defined Vehicle)에 의한 소프트웨어 통합이 진행됨에 따라 차량 제어기의 소프트웨어 복잡도가 증가하고 있습니다. 이러한 복잡성에 대응하기 위한 접근 방법 중 하나가 컨테이너 기술의 도입입니다.
그러나 기존 OCI 표준을 준수하는 컨테이너 솔루션들은 제한된 성능과 차량 시스템 고유의 요구사항을 충분히 만족하지 못하여 차량 제어기 환경에서의 사용에는 한계가 있습니다.
이러한 한계를 해결하기 위해 고성능·보안 강화 컨테이너 솔루션으로서 **SSAM**을 개발하였습니다. **SSAM**은 게이트웨이 제어기 환경에서 단일 어플리케이션 기준 100밀리초 이내에 컨테이너 환경을 구성하고 실행할 수 있습니다. 또한 컨테이너 패키지의 무결성을 지속적으로 검증하여 변조나 비인가 수정을 실시간으로 탐지할 수 있습니다.

## Features

- **Package Management**: Package의 설치, 제거, 업그레이드 등을 지원합니다.
- **Integrity Verification**: Linux의 `dm-verity`와 `EROFS`를 활용하여 무결성을 보장합니다.
- **Container Execution**: Systemd를 활용하여 OCI 호환 container runtime (`crun`)을 통해 Container를 실행합니다.
- **Resource Isolation**: OCI Runtime이 제공하는 리소스 격리 기능에 더해, 복수의 Container-native 앱이 공용으로 사용하는 데이터 영역에 대해 ext4 파일 시스템의 project quota 설정을 지원합니다.

## 주요 구성요소

* [`ssamd`](ssamd): Package 관리, Container 실행환경 구성 및 실행을 담당하는 데몬.
* [`ssam`](ssam): `ssamd`와의 통신을 할 수 있는 Command-line interface.
* [`ssam-wrap`](ssam-wrap): Docker image에서의 변환 기능을 포함하는 SSAM 패키지 생성도구.

## Requirements

### Runtime requirements
- Linux kernel: dm-verity, container, erofs, ext4 project quota, bridge 네트워킹(`bridge`/`veth`/network namespace), netfilter NAT(`nf_tables`/`nf_nat`/`nf_conntrack`/`nft_nat`/`nft_masq`) 관련 설정 필요
- OCI runtime: [`crun`](https://github.com/containers/crun)과 테스트되었습니다.
- Bridge 네트워크 모드 (`[container.network] mode = "bridge"`): [`nft`](https://www.netfilter.org/projects/nftables/) (nftables) 필요

### Build requirements
- Rust 1.88.0+ (2024 edition)

### `ssam-wrap` requirements
- `lz4`
- `erofs-utils`
- `fakeroot`
- [`cryptsetup`](https://gitlab.com/cryptsetup/cryptsetup)
- Docker 이미지 변환 기능 사용 시
  - [`umoci`](https://github.com/opencontainers/umoci)
  - [`skopeo`](https://github.com/containers/skopeo) >= 1.15

## Quick-start Guide

### 빌드 및 기본 설정
```
$ cargo install --path ssam-wrap
$ cargo install --path ssam
$ cargo install --path ssamd

$ mkdir -p ~/ssam/{keys,bundled,downloaded,mnt,data}
$ ssamd/releasedata/gen_ssamd_conf.sh \
      --output-path ~/.cargo/bin \
      --bundled-pkgs-dir ~/ssam/bundled \
      --downloaded-pkgs-dir ~/ssam/downloaded \
      --pkgs-mnt-root ~/ssam/mnt \
      --pkgs-data-root ~/ssam/data \
      --public-key-file ~/ssam/keys/public_key.pem
```

### 패키지 검증용 키쌍 생성
```
$ cd ~/ssam/keys/
$ openssl genpkey -algorithm RSA -out private_key.pem -pkeyopt rsa_keygen_bits:2048
$ openssl rsa -in private_key.pem -pubout -out public_key.pem
```

### 샘플 SSAM 패키지 생성
#### Docker 이미지를 변환하여 생성
* Docker 이미지 변환을 위해서는 [`ssam-wrap` requirements](#ssam-wrap-requirements)에 명시된 `umoci`와 `skopeo`가 필요함에 주의
* 패키지 준비
```
$ cd ~/ssam/
$ mkdir docker-helloworld
$ cd docker-helloworld
$ ssam-wrap --prepare docker-helloworld --pkgfs-src docker://docker.io/hello-world:latest .
```

* 패키지 설정
```diff
--- config.toml.orig	2026-05-13 18:20:44.438136264 +0900
+++ config.toml	2026-05-13 18:20:54.812170283 +0900
@@ -12,8 +12,8 @@
 # data_dirs = "/container/path1:container/path2:container/path3"
 
 [container.security]
-seccomp = true # default is true
-mac = true # default is true
+seccomp = false # default is true
+mac = false # default is true
 
 [service]
 service_type = "simple"
```

* 패키지 filesystem 구성
```
$ mkdir pkgfs/etc
$ cp /etc/resolv.conf pkgfs/etc/
```

* 패키지 생성
```
$ ssam-wrap --private-key ~/ssam/keys/private_key.pem --output docker-helloworld.ssam .
$ cp docker-helloworld.ssam ~/ssam/bundled/
```

#### From scratch
* 패키지 준비
```
$ cd ~/ssam/
$ mkdir helloworld
$ ssam-wrap --prepare helloworld helloworld/
$ cd helloworld/
$ cargo new --name helloworld app/
$ cargo build --manifest-path app/Cargo.toml --target x86_64-unknown-linux-musl --release
$ cp app/target/x86_64-unknown-linux-musl/release/helloworld pkgfs/
```

* 패키지 설정
```diff
--- config.toml.orig	2026-05-13 18:20:44.438136264 +0900
+++ config.toml	2026-05-13 18:20:54.812170283 +0900
@@ -12,8 +12,8 @@
 # data_dirs = "/container/path1:container/path2:container/path3"
 
 [container.security]
-seccomp = true # default is true
-mac = true # default is true
+seccomp = false # default is true
+mac = false # default is true
 
 [service]
 service_type = "simple"
```

* 컨테이너 실행 설정
```diff
--- runtime.json.orig	2026-05-13 17:03:47.594167700 +0900
+++ runtime.json	2026-05-13 16:22:08.634277789 +0900
@@ -7,7 +7,7 @@
       "gid": 0
     },
     "args": [
-      "/sh"
+      "/helloworld"
     ],
     "env": [
       "PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
```

* 패키지 생성
```
$ ssam-wrap --private-key ~/ssam/keys/private_key.pem --output helloworld.ssam .
$ cp helloworld.ssam ~/ssam/bundled/
```

### 실행

#### `ssamd` 실행
```
$ cd ~/.cargo/bin/
$ sudo ./ssamd
[INFO] Successfully loaded runtime configuration from '~/.cargo/bin/ssamd.toml'
...
```

#### 패키지 실행 확인
```
$ ssam list
List of packages
Package name         Status
===============================================
helloworld           Ready
docker-helloworld    Ready

$ journalctl --no-pager -u docker-helloworld
systemd[1]: Started A short description of the package.
crun[]: Hello from Docker!
crun[]: This message shows that your installation appears to be working correctly.
crun[]: To generate this message, Docker took the following steps:
crun[]:  1. The Docker client contacted the Docker daemon.
crun[]:  2. The Docker daemon pulled the "hello-world" image from the Docker Hub.
crun[]:     (arm64v8)
crun[]:  3. The Docker daemon created a new container from that image which runs the
crun[]:     executable that produces the output you are currently reading.
crun[]:  4. The Docker daemon streamed that output to the Docker client, which sent it
crun[]:     to your terminal.
crun[]: To try something more ambitious, you can run an Ubuntu container with:
crun[]:  $ docker run -it ubuntu bash
crun[]: Share images, automate workflows, and more with a free Docker ID:
crun[]:  https://hub.docker.com/
crun[]: For more examples and ideas, visit:
crun[]:  https://docs.docker.com/get-started/
systemd[1]: docker-helloworld.service: Deactivated successfully.


$ journalctl --no-pager -u helloworld
systemd[1]: Started A short description of the package.
crun[]: Hello, world!
systemd[1]: helloworld.service: Deactivated successfully.
```

## TODOs
* 상세 문서 제공
* 네트워크 설정 기능 제공

## License

Apache License 2.0 - See [LICENSE](LICENSE) for details.

Copyright 2025-2026 Hyundai Mobis Co., Ltd.
