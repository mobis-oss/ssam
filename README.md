# SSAM
* [Ssam](https://en.wikipedia.org/wiki/Ssam)
* SSAM is a solution developed to enable fast execution of container-native apps and continuous runtime integrity verification in resource-constrained embedded environments such as automotive systems.
* SSAM runs container-native apps using the OCI Runtime standard.
* SSAM defines packages that leverage filesystem images to configure the runtime environment for container-native apps.
* SSAM provides simplified container package definition, management, and execution for embedded Linux environments.
* SSAM supports runtime integrity verification of containers through `dm-verity`.

## Background
As software integration driven by *SDV* (Software-Defined Vehicles) advances, the software complexity of automotive controllers is increasing. One of the approaches to address this growing complexity is the adoption of container technology.
However, container solutions that comply with the conventional OCI standards fall short in automotive controller environments, as they suffer from limited performance and do not adequately meet the specific requirements of automotive systems.
To address these limitations, a high-performance, security-enhanced container solution was developed — **SSAM**. In a gateway controller environment, **SSAM** can establish a container environment and execute a single application within 100 milliseconds. It also continuously verifies the integrity of container packages, enabling real-time detection of any tampering or unauthorized modifications.

## Features

- **Package Management**: Supports installation, removal, and upgrade of packages.
- **Integrity Verification**: Ensures integrity using Linux `dm-verity` and `EROFS`.
- **Container Execution**: Runs containers via an OCI-compatible container runtime (`crun`) using Systemd.
- **Resource Isolation**: In addition to the resource isolation provided by OCI Runtime, supports ext4 project quota configuration for data areas shared by multiple container-native apps.

## Key Components

* [`ssamd`](ssamd): Daemon responsible for package management, container runtime environment configuration, and execution.
* [`ssam`](ssam): Command-line interface for communicating with `ssamd`.
* [`ssam-wrap`](ssam-wrap): SSAM package creation tool with Docker image conversion support.

## Requirements

### Runtime requirements
- Linux kernel: dm-verity, container, erofs, and ext4 project quota configurations required
- OCI runtime: Tested with [`crun`](https://github.com/containers/crun).

### Build requirements
- Rust 1.88.0+ (2024 edition)

### `ssam-wrap` requirements
- `lz4`
- `erofs-utils`
- `fakeroot`
- [`cryptsetup`](https://gitlab.com/cryptsetup/cryptsetup)
- For Docker image conversion:
  - [`umoci`](https://github.com/opencontainers/umoci)
  - [`skopeo`](https://github.com/containers/skopeo) >= 1.15

## Quick-start Guide

### Build and basic setup
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

### Generate key pair for package verification
```
$ cd ~/ssam/keys/
$ openssl genpkey -algorithm RSA -out private_key.pem -pkeyopt rsa_keygen_bits:2048
$ openssl rsa -in private_key.pem -pubout -out public_key.pem
```

### Create sample SSAM packages
#### From a Docker image
* Note that `umoci` and `skopeo` listed in [`ssam-wrap` requirements](#ssam-wrap-requirements) are required for Docker image conversion
* Prepare the package
```
$ cd ~/ssam/
$ mkdir docker-helloworld
$ cd docker-helloworld
$ ssam-wrap --prepare docker-helloworld --pkgfs-src docker://docker.io/hello-world:latest .
```

* Configure the package
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

* Set up the package filesystem
```
$ mkdir pkgfs/etc
$ cp /etc/resolv.conf pkgfs/etc/
```

* Build the package
```
$ ssam-wrap --private-key ~/ssam/keys/private_key.pem --output docker-helloworld.ssam .
$ cp docker-helloworld.ssam ~/ssam/bundled/
```

#### From scratch
* Prepare the package
```
$ cd ~/ssam/
$ mkdir helloworld
$ ssam-wrap --prepare helloworld helloworld/
$ cd helloworld/
$ cargo new --name helloworld app/
$ cargo build --manifest-path app/Cargo.toml --target x86_64-unknown-linux-musl --release
$ cp app/target/x86_64-unknown-linux-musl/release/helloworld pkgfs/
```

* Configure the package
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

* Configure the container runtime
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

* Build the package
```
$ ssam-wrap --private-key ~/ssam/keys/private_key.pem --output helloworld.ssam .
$ cp helloworld.ssam ~/ssam/bundled/
```

### Running

#### Start `ssamd`
```
$ cd ~/.cargo/bin/
$ sudo ./ssamd
[INFO] Successfully loaded runtime configuration from '~/.cargo/bin/ssamd.toml'
...
```

#### Verify package execution
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
* Provide detailed documentation
* Provide network configuration support

## License

Apache License 2.0 - See [LICENSE](LICENSE) for details.

Copyright 2025-2026 Hyundai Mobis Co., Ltd.
