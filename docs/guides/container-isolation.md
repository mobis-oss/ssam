# SSAM Container Isolation

<p>
  <a href="container-isolation.ko.md"><img src="https://img.shields.io/badge/lang-한국어-blue" alt="Korean"></a>
</p>

When you run a container with SSAM, you can configure its **network** and **resource limits (cgroup and storage quota)**.
This guide covers how to set them up.

> 📖 Restricting a running container's behavior (seccomp, AppArmor) is covered by the [Container Hardening](container-hardening.md) guide; package integrity (signing, dm-verity, read-only filesystem) by the [Package Integrity](package-integrity.md) guide.

> 📖 For how isolation is delegated to and enforced by the *OCI Runtime* (the design and model), see the [Container Isolation design doc](../architecture/container-isolation.md).

---

## Table of Contents

1. [Networking](#networking)
   - [Host networking](#host-networking)
   - [No networking](#no-networking)
   - [Bridge networking](#bridge-networking)
   - [Using predefined networking](#using-predefined-networking)
2. [Container cgroup](#container-cgroup-packagecgroup)
3. [Storage quota](#storage-quota-storage_limit)

---

## Networking

Network modes are chosen per package via `mode` in [`[container.network]`](../reference/package-config.md#containernetwork).

| Mode | What the container gets |
| --- | --- |
| **`host`** | Shares the host network namespace — no isolation. |
| **`none`** | No networking. |
| **`bridge`** | Attached to an internal bridge — containers on the same bridge can talk. **Requires daemon bridge configuration.** |
| **an absolute path** | Attaches to a specific externally-managed network namespace, outside `ssamd`. |

### Host networking

Set `mode` to `host` in `config.toml` (or omit `[container.network]`):

```toml
[container.network]
mode = "host"
```

- The container shares the host's network namespace directly.
- There is no isolation and the lowest overhead; this is the default when `[container.network]` is omitted.
- `ssamd` needs no extra configuration.

### No networking

Set `mode` to `none` in `config.toml`:

```toml
[container.network]
mode = "none"
```

- The container gets an isolated, empty network namespace.
- No connectivity — to outside networks or to other containers.
- `ssamd` needs no extra configuration.

### Bridge networking

#### Overview

`bridge` mode places the container on an **internal** bridge managed by `ssamd`:

- **Isolated.** No gateway, NAT, or external routing — a bridged container reaches other containers on the same bridge, but not outside networks.
- **Shared networks.** Packages that set the same [`network_name`](../reference/package-config.md#containernetworkbridge) join the same bridge and can reach each other; omitting it gives the package a private bridge of its own.
- **Addressing.** `ssamd` allocates each container an address from the daemon's bridge address pool, and names the bridge interface automatically.
- **Requires daemon bridge configuration.** If the daemon's bridge configuration ([`[network.bridge]`](../reference/daemon-config.md#networkbridge)) is disabled while a package requests `bridge`, `ssamd` **rejects** the package.

Bridge mode needs both the **daemon** and the **package** sides configured.

#### ssamd configuration

Enable the managed bridge in `ssamd.toml`:

```toml
[network.bridge]
enabled = true

[network.bridge.addr_pool]      # optional
base = "172.20.0.0/16"          # address pool (default: 172.20.0.0/16)
size = 29                       # per-bridge subnet prefix (default: 29)
```

Bridge mode also needs the host's `bridge` / `veth` kernel features.

> 📖 For all keys see the [`[network.bridge]` reference](../reference/daemon-config.md#networkbridge); for the operational walkthrough — enabling the bridge on a device and applying it — see [Serving SSAM → Networking](serving-ssam.md#networking).

#### Per-package configuration

Declare the mode and optional bridge settings in `config.toml`:

```toml
[container.network]
mode = "bridge"

[container.network.bridge]      # optional
network_name   = "shared-net-a" # optional (default: dedicated bridge) — share a bridge across packages
interface_name = "eth0"         # optional (default: eth0) — container-side interface name
```

> 📖 For all keys see the [`[container.network]` reference](../reference/package-config.md#containernetwork).

### Using predefined networking

SSAM also lets a package use a network you have predefined on the host.
For example, if you set up a network for the container on the host like this:

```console
# ip netns add my-netns
# ip netns exec my-netns ip link set lo up
```

point the package's `config.toml` `mode` at its path when creating the package:

```toml
[container.network]
mode = "/var/run/netns/my-netns"
```

- The container uses the configured namespace at run time, and the run fails if it has not been created beforehand.
- The host is responsible for preparing that namespace, so the package ends up depending on a system with that specific setup.
- `ssamd` needs no extra configuration.

---

## Container cgroup (`package.cgroup`)

[`package.cgroup`](../reference/daemon-config.md#cgroup) in `ssamd.toml` sets the **root cgroup** all package containers are placed under.

- **Left unset (empty, the default)**, `ssamd` uses the host's `system.slice` as the root cgroup.
- **Set to a value**, `ssamd` uses that path as the root cgroup.

Once the root cgroup is decided, `ssamd` sets each container's *OCI Runtime config* `cgroupsPath` to `<package.cgroup>/<package-name>.service/container` — letting you cap the resources all package containers use together.
The root cgroup **must exist before `ssamd` starts**; `ssamd` does not create it (the daemon aborts if it is missing).

There are two ways to set it.
Either way, the host prepares the root cgroup in advance and `ssamd.toml` points at its name.

### Using a systemd slice unit

You can use a *systemd* **slice unit** as the root cgroup.
For example, with `ssam.slice` as the slice to use as the root cgroup:
- set `cgroup = "ssam.slice"` under `[package]` in `ssamd.toml`
- `ssamd` sets each container's service unit (`<package-name>.service`) `Slice=` to `ssam.slice`
- `ssamd` sets each container's *OCI Runtime config* `cgroupsPath` to `ssam.slice/<package-name>.service/container`

Prepare the slice on the host in advance — for example:

```console
# cat > /etc/systemd/system/ssam.slice <<'EOF'
[Unit]
Description=SSAM package containers
EOF
# systemctl daemon-reload
# systemctl start ssam.slice
```

> 📖 For writing slice units and per-slice resource limits (`CPUQuota=`, `MemoryMax=`, …), see the `systemd.slice(5)` and `systemd.resource-control(5)` man pages.

### Using a cgroup

You can also create a cgroup directly and use it as the root.
For example, with `ssam` as the name to use as the root cgroup:
- set `cgroup = "ssam"` under `[package]` in `ssamd.toml`
- `ssamd` sets each container's *OCI Runtime config* `cgroupsPath` to `ssam/<package-name>.service/container`
- since this value is not a *systemd* slice, `ssamd` does not set `Slice=` on the service unit and logs a warning

Prepare the cgroup on the host in advance — for example:

```console
# mkdir -p /sys/fs/cgroup/ssam
```

> 📖 For setting **resource limits** (CPU, memory, …) on the cgroup, see the kernel [cgroup v2 docs](https://docs.kernel.org/admin-guide/cgroup-v2.html) or the `cgroups(7)` man page.

---

## Storage quota (`storage_limit`)

When a package declares [`container.storage_limit`](../reference/package-config.md#storage_limit), `ssamd` applies a size cap to that package's data directory.
`ssamd` implements this with *ext4* **project quota**.
To use it, the **host system's kernel configuration** and the **filesystem set as [`package.dir.data`](../reference/daemon-config.md#data)** must each provide the following:

**Kernel configuration**

- `CONFIG_EXT4_FS` — the *ext4* filesystem
- `CONFIG_QUOTA` — disk quota support
- `CONFIG_QFMT_V2` — project quota support (vfsv1 quota format)

**Filesystem**

- an **ext4** filesystem
- **project quota enabled** — e.g. format it with `mkfs.ext4 -O quota,project` (or add the feature with `tune2fs`) and mount it with project quota (`-o prjquota`)

If these are not met (not *ext4*, or project quota not enabled), `ssamd` logs a warning and continues **without** applying `storage_limit`; the container starts with no size cap.
To actually enforce the cap, provision the above ahead of time.
