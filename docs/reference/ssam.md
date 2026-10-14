# ssam CLI Reference

<p>
  <a href="ssam.ko.md"><img src="https://img.shields.io/badge/lang-한국어-blue" alt="Korean"></a>
</p>

`ssam` is the command-line client for the SSAM daemon (`ssamd`).
It manages packages on a running daemon — installing, starting, stopping, removing, and inspecting them — by calling `ssamd`'s [Remocon gRPC API](remocon-rpc.md).

> **`ssamd` must be running and reachable.** `ssam` is only a client; see [Serving SSAM](../guides/serving-ssam.md) for running the daemon.

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

`ssam` connects to `ssamd` over *gRPC* at the fixed Linux abstract Unix domain socket `@ssamd`:

```
unix://\0ssamd
```

| Setting | Value |
| --- | --- | --- |
| Endpoint | `unix://\0ssamd` |
| Transport | local gRPC over an abstract Unix domain socket |
| Authorization | one of the peer's effective or supplementary GIDs must match the daemon's effective GID or a GID in `[rpc].allowed_gids` via `SO_PEERCRED` and `SO_PEERGROUPS` |

One of the peer's effective or supplementary GIDs matching the daemon's effective GID is always allowed.
Additional peer effective or supplementary GIDs can be configured with `[rpc].allowed_gids`; the list defaults to empty.
UID is not used for authorization.

For the control API details, see the [Remocon gRPC control API](remocon-rpc.md).

---

## Common workflows

Install a package, run it, check it, and remove it:

```console
$ ssam install ./helloworld-1.0.0.ssam   # verify + install from a SSAM package file
$ ssam start helloworld                   # start the container
$ ssam list                               # confirm it is Running
$ ssam stop helloworld                    # stop it
$ ssam remove helloworld                  # uninstall
```

Container stdout/stderr and lifecycle appear in `journalctl` under the package's *systemd* unit (see [Serving SSAM → Verify](../guides/serving-ssam.md#step-6--verify)).

---

## Commands

Global form:

```
ssam <COMMAND> [ARGS...]
```

### Global option

| Option | Description |
| --- | --- |
| `-V`, `--version` | Print the version declared by the `ssam` workspace package without connecting to `ssamd`. |

### `list`

List all packages and their status.

```
ssam list [--detail]
```

| Option | Description |
| --- | --- |
| `--detail` | Show detailed information for each package (path, version, description, quota). |

```console
$ ssam list
List of packages
Package name         Status
===============================================
helloworld           Ready
```

Status values correspond to the daemon's [package states](../architecture/execution.md#package-state-machine) (`Ready`, `Running`, `SettingUp`, `Broken`, …).

### `start`

Start one or more installed packages.

```
ssam start <PACKAGE_NAMES>...
ssam start --all
```

| Argument / option | Description |
| --- | --- |
| `<PACKAGE_NAMES>...` | One or more package names to start. |
| `--all` | Start **all** installed packages. Mutually exclusive with named packages. |

Either a package list **or** `--all` is required — a bare `ssam start` is rejected, so you cannot accidentally act on everything.
Per-package results are printed as `[OK] <name>` or `[FAIL] <name>: <message>`.

```console
$ ssam start helloworld web-app
  [OK] helloworld
  [OK] web-app
```

### `stop`

Stop one or more running packages.
Same argument shape as `start`.

```
ssam stop <PACKAGE_NAMES>...
ssam stop --all
```

| Argument / option | Description |
| --- | --- |
| `<PACKAGE_NAMES>...` | One or more package names to stop. |
| `--all` | Stop **all** installed packages. Mutually exclusive with named packages. |

### `install`

Install a package from a local SSAM package file.
The daemon verifies the package's signature and integrity before installing.

```
ssam install <PACKAGE_PATH> [-d|--downgrade] [-r|--remove_data]
```

| Argument / option | Description |
| --- | --- |
| `<PACKAGE_PATH>` | Path to the SSAM package file. |
| `-d`, `--downgrade` | Allow installing a package that is a **lower version** than the one already installed. |
| `-r`, `--remove_data` | Remove all data associated with the package during (re)install. |

A relative `<PACKAGE_PATH>` is resolved against the local filesystem because the client and daemon use the same local transport.

```console
$ ssam install ./helloworld-1.0.0.ssam
```

### `remove`

Remove (uninstall) an installed package by name.

```
ssam remove <PACKAGE_NAME>
```

| Argument | Description |
| --- | --- |
| `<PACKAGE_NAME>` | Name of the installed package to remove. |

```console
$ ssam remove helloworld
```

### `wipe-data`

Wipe the data volumes of one or more installed packages, keeping them installed.
Only a package that is set up and stopped (`Ready`) or cleaned up (`Cleaned`) can be wiped; other states, including `Verified` before setup, are refused.
Same argument shape as `start`/`stop`.

```
ssam wipe-data <PACKAGE_NAMES>...
ssam wipe-data --all
```

| Argument / option | Description |
| --- | --- |
| `<PACKAGE_NAMES>...` | One or more package names to wipe data for. |
| `--all` | Wipe data for **all** installed packages. Mutually exclusive with named packages. |

```console
$ ssam wipe-data helloworld
```

### `pkg-info`

Show information about one or more packages, referenced by URI.

```
ssam pkg-info <PACKAGE_URI>...
```

| URI form | Meaning |
| --- | --- |
| `ssam://<name>` | An **installed** package, by name. |
| `file:///<absolute-path>` | A SSAM package **file on disk** (not necessarily installed). The path must be absolute. |

```console
$ ssam pkg-info ssam://helloworld
$ ssam pkg-info file:///home/user/helloworld-1.0.0.ssam
```

### `timeline-info`

Show the daemon's timeline / uptime information.

```
ssam timeline-info
```

---

> 📖 For the underlying request/response contract these commands map onto, see the [Remocon gRPC Control API Reference](remocon-rpc.md).
