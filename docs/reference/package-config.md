# Package Configuration (`config.toml`) Reference

<p>
  <a href="package-config.ko.md"><img src="https://img.shields.io/badge/lang-한국어-blue" alt="Korean"></a>
</p>

`config.toml` is the **package configuration** for a SSAM package. It carries
the package's identity (name, version), its data/quota and network settings,
its security posture, and the service model `ssamd` uses to run its container.

> 📖 For a step-by-step walkthrough of filling in `config.toml`, see the
> **[Wrapping a SSAM — Package Creation Guide](../guides/wrapping-a-ssam.md)**.

`ssam-wrap` generates a template `config.toml` in [prepare mode](ssam-wrap.md#command-line-reference)
(`--prepare <NAME>`).

> **Where values are checked.** `ssam-wrap` validates the package **version**
> (must be valid [semver](https://semver.org/)) at wrap time. Most other
> fields — network `mode`, `service_type` — are consumed and
> validated by `ssamd` when the package is loaded or started. An invalid value
> that slips past `ssam-wrap` will surface as a load/run error on the target.

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
autostart = true                # optional

[container]
storage_limit = 1000            # optional, kilobytes
data_dirs = "/path1:/path2"     # optional

[container.security]
seccomp = true
mac = true

[container.network]             # optional table
mode = "host"                   # optional

[container.network.bridge]      # optional, only for mode = "bridge"
network_name = "shared-net-a"   # optional
interface_name = "eth0"         # optional

[service]
service_type = "simple"
bus_name = "my.bus.name"        # optional
remain_after_exit = false       # optional
```

The `[package]`, `[container]`, `[container.security]`, and `[service]` tables
are **mandatory**. The `[container.network]` table (and its `bridge` subtable)
is **optional**.

---

## `[package]`

Package identity and lifecycle.

### `name`

- **Type:** string · **Required**
- The package name. Used as the identity under which `ssamd` installs, lists,
  and runs the package (it becomes the *systemd* unit name), so it should be
  unique and filesystem-friendly.
- In prepare mode, `ssam-wrap` pre-fills this with the `<NAME>` you passed to
  `--prepare`.

### `description`

- **Type:** string · **Required**
- A human-readable description. Surfaces in package-query *gRPC* responses or as
  the *systemd* service description on the target.

### `version`

- **Type:** string · **Required**
- The package version. **Must be valid [semver](https://semver.org/)**
  (e.g. `1.0.0`, `0.2.1-rc.1`). `ssam-wrap` rejects the build with an error
  otherwise.
- Used for upgrade comparisons and for the default output filename
  `<name>-<version>.ssam`.

### `autostart`

- **Type:** boolean · **Optional** · **Default:** `true`
- Whether `ssamd` starts the package automatically when it is loaded (e.g. at
  boot). Set to `false` for packages whose start you manage separately through a
  Remocon client.

---

## `[container]`

Data-area and storage settings for the running container.

### `storage_limit`

- **Type:** integer (kilobytes) · **Optional**
- A quota, in **kilobytes**, applied to the package's writable data area via
  *ext4* **project quota**. Omit for no quota.
- Enforcement requires *ext4* project-quota support on the target kernel
  (listed as optional in the `ssamd` requirements). Without it, the limit is
  not applied.

### `data_dirs`

- **Type:** string (colon-separated paths) · **Optional**
- A `:`-separated list of **absolute container paths that must be writable**
  inside the container. The package filesystem itself is read-only
  (integrity-verified), so anything the app needs to write to must be declared here.
- `ssamd` backs these paths with persistent data directories under
  [`packages_data_root`](daemon-config.md#packages_data_root), bind-mounts them
  into the container, and caps usage with [`storage_limit`](#storage_limit). For
  the exact acquire/mount steps, see
  [Execution](../architecture/execution.md#the-execution-pipeline).

```toml
data_dirs = "/app/data:/app/logs"
```

---

## `[container.security]`

Security hardening toggles.

### `seccomp`

- **Type:** boolean · **Required** · **Recommended:** `true`
- When `true`, the container runs under the *seccomp* policy bundled in the
  package.
- The policy ships as a `seccomp.json` file in the package. Edit the template
  `seccomp.json` that `ssam-wrap --prepare` provides, or write your own.
- Disable only for debugging or when a workload genuinely needs unfiltered
  syscalls.

### `mac`

- **Type:** boolean · **Required** · **Recommended:** `true`
- Enables Mandatory Access Control (MAC) confinement for the container. MAC is
  provided via *AppArmor*, and an *AppArmor* profile under a **predefined name**
  must be loaded on the target.
- For the profile naming convention and how to author/load it, see
  [Container Isolation](../architecture/container-isolation.md) and the
  [Container Hardening guide](../guides/container-hardening.md).

---

## `[container.network]`

Container networking. The **entire table is optional** — omit it to accept the
default (`host`) networking.

### `mode`

- **Type:** string · **Optional** · **Default:** `host`
- One of:
  - **`host`** — the container shares the host network namespace. No isolation;
    lowest overhead. This is the default when `mode` is absent.
  - **`none`** — no network connectivity; the container gets an isolated,
    empty network namespace.
  - **`bridge`** — the container gets its own network namespace attached to an
    internal bridge (no gateway, NAT, or external routing). Containers sharing a
    bridge reach each other but not outside networks. See
    [`[container.network.bridge]`](#containernetworkbridge).
  - **an absolute path** — the path to an externally set-up and managed network
    namespace. The container uses that namespace at run time; if no namespace
    exists at the path, the run fails.

A value that is neither an absolute path nor one of `host`/`none`/`bridge` is
rejected.

> ⚠️ **Bridge needs the daemon bridge:** if a package requests `bridge` but the
> daemon's bridge network is disabled, `ssamd` **rejects** the package.

---

## `[container.network.bridge]`

Only meaningful with `mode = "bridge"`. Both keys are optional.

### `network_name`

- **Type:** string · **Optional**
- Names a shared bridge. Packages that set the same `network_name` join the same
  internal bridge and can reach each other; when omitted, the package gets a
  private bridge of its own.

### `interface_name`

- **Type:** string · **Optional**
- The name of the network interface inside the container. Provided for apps that
  expect a specific interface name. Defaults to `eth0` when omitted.

---

## `[service]`

How `ssamd` runs the container as a *systemd*-managed service.

### `service_type`

- **Type:** string · **Required**
- The *systemd* service model. Maps directly to the unit's `Type=`. One of:

| Value | systemd `Type=` | Notes |
| --- | --- | --- |
| `simple` | `simple` | Long-running process; considered started immediately. |
| `exec` | `exec` | Like `simple`, but started only after the binary has been executed. |
| `forking` | `forking` | The process forks and the parent exits. |
| `oneshot` | `oneshot` | Runs to completion once; often paired with `remain_after_exit`. |
| `dbus` | `dbus` | Ready once it acquires `bus_name` on the D-Bus system bus. Should be paired with `bus_name` (not enforced). |
| `notify` | `notify` | Ready once it sends `sd_notify`. `ssamd` sets `NotifyAccess=all`; also requires a `/run` mount in `runtime.json`. |
| `idle` | `idle` | Like `simple`, but execution is delayed until other jobs are dispatched. |

> 📖 For each *systemd* service type in detail, see the *systemd.service(5)* man page.

> ⚠️ **`notify` services need `/run`.** When `service_type = "notify"`,
> the `runtime.json` must declare a `/run` mount; `ssam-wrap` aborts the wrap
> with an error if it is missing. See the
> [runtime config notes](../guides/wrapping-a-ssam.md#step-4--configure-the-runtime-runtimejson).

### `bus_name`

- **Type:** string · **Optional**
- The *D-Bus* name the service acquires. Used to populate the unit's `BusName=`.
  Should be set when `service_type = "dbus"` — nothing validates
  this, so a missing `bus_name` silently produces a `dbus`-type unit with no
  `BusName=` set, rather than being rejected. Ignored for other service types.

```toml
bus_name = "com.example.MyService"
```

### `remain_after_exit`

- **Type:** boolean · **Optional** · **Default:** `false`
- Maps to *systemd* `RemainAfterExit=`. When `true`, the service is considered
  active even after its main process exits — commonly used with
  `service_type = "oneshot"`.

---

## Required vs. optional keys

| Key | Required? | Default |
| --- | --- | --- |
| `package.name` | ✅ Required | — |
| `package.description` | ✅ Required | — |
| `package.version` | ✅ Required (valid semver) | — |
| `package.autostart` | Optional | `true` |
| `container.storage_limit` | Optional | no quota |
| `container.data_dirs` | Optional | none |
| `container.security.seccomp` | ✅ Required | (recommended `true`) |
| `container.security.mac` | ✅ Required | (recommended `true`) |
| `container.network` (table) | Optional | host networking |
| `container.network.mode` | Optional | `host` |
| `container.network.bridge.network_name` | Optional | private bridge |
| `container.network.bridge.interface_name` | Optional | `eth0` |
| `service.service_type` | ✅ Required | — |
| `service.bus_name` | Optional (should be set for `dbus`, not enforced) | none |
| `service.remain_after_exit` | Optional | `false` |

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
storage_limit = 50000            # 50 MB data-area quota (project quota)
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
service_type = "notify"          # requires a /run mount in runtime.json
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

> 📖 For the full packaging workflow, see the
> [ssam-wrap Command Reference](ssam-wrap.md).
