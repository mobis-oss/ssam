# SSAM Container Hardening

<p>
  <a href="container-hardening.ko.md"><img src="https://img.shields.io/badge/lang-한국어-blue" alt="Korean"></a>
</p>

This guide covers two mechanisms that limit **what a running SSAM container can do** to the kernel and the host.
It describes how operators and package authors configure each:

- **System-call filtering (seccomp)** — limits the syscalls a container may make.
- **Mandatory access control (AppArmor)** — limits what a container may touch (files, capabilities, network).

> 📖 Package integrity (signing, dm-verity, read-only filesystem) is covered by the [Package Integrity](package-integrity.md) guide; network and resource isolation by the [Container Isolation](container-isolation.md) guide.

> 📖 For how seccomp and MAC are delegated to and enforced by the *OCI Runtime* (the design and model), see the [Container Isolation design doc](../architecture/container-isolation.md).

---

## Table of Contents

1. [System-call filtering — seccomp](#system-call-filtering--seccomp)
2. [Mandatory access control — AppArmor](#mandatory-access-control--apparmor)

---

## System-call filtering — seccomp

*seccomp* limits which Linux **system calls** a container may make, narrowing the paths an attacker could use to reach the kernel.
SSAM controls it with [`container.security.seccomp`](../reference/package-config.md#seccomp) and the `seccomp.json` policy file carried in the package.

The policy file is **part of the signed metadata**, so it cannot be altered after signing.

**How to configure.**

1. **Generate the template.** `ssam-wrap --prepare <name>` generates a `seccomp.json` in the workspace.
   The template is **Docker's default seccomp profile**, using the standard `SCMP_ACT_*` action · `archMap` · `syscalls` schema shared with the [OCI seccomp configuration](https://github.com/opencontainers/runtime-spec/blob/main/config-linux.md#seccomp).
   An existing `seccomp.json` is not overwritten.
2. **Edit if needed.** Most packages can use the template unchanged.
   To permit or deny specific syscalls, edit the `syscalls` entries or write your own policy.
3. **Enable.** Set `seccomp = true` (recommended) in `config.toml`.
   `false` applies no filter — use only for debugging.
4. **Wrap.** `ssam-wrap` embeds `seccomp.json` in the package and signs it.

**For example.**

To stop an app inside the container from changing file modes, block `chmod` by removing its name from the `seccomp.json` template's allow list (`SCMP_ACT_ALLOW`).
Because the default action is `SCMP_ACT_ERRNO` (deny), any syscall not on the list is blocked:

```diff
 {
-  "names": ["chmod", "fchmod", "fchmodat"],
+  "names": ["fchmod", "fchmodat"],
   "action": "SCMP_ACT_ALLOW"
 }
```

Now the container's `chmod(2)` call is denied — the caller gets an error (`ENOSYS`).

At runtime `ssamd` injects the policy into the container's *OCI Runtime config*.
If the policy has an entry it cannot map, it **fails the package** rather than running it unfiltered.

---

## Mandatory access control — AppArmor

Mandatory access control (MAC) limits what a container process can touch (files, capabilities, network, …) by policy.
SSAM applies it with an *AppArmor* profile, controlled by [`container.security.mac`](../reference/package-config.md#mac).

**SSAM *selects* an AppArmor profile by name.** Authoring a profile and loading it into the kernel is the **host's responsibility**.
The host must therefore load the profile in advance, e.g. as part of the image build or device provisioning.

> 📖 The `[container.security]` fields are documented in the [package config reference](../reference/package-config.md#containersecurity).

When `mac = true`, `ssamd`:

1. Looks for a profile named **`ssam-prof-<package-name>`** (the package's `name` from `config.toml`).
2. If it is loaded, applies it to the container.
3. If it is **not** loaded, falls back to **`container-default`**.
4. Refuses to start the container if *AppArmor* is not enabled on the host, the loaded-profile list cannot be read, or neither profile is loaded.

**How to configure.**

1. Set `mac = true` in `config.toml`.
2. Author the **`ssam-prof-<package-name>`** profile (`<package-name>` is the [`name`](../reference/package-config.md#name) in `config.toml`).
   A skeleton for `helloworld`:

   ```
   #include <tunables/global>

   profile ssam-prof-helloworld flags=(attach_disconnected) {
     #include <abstractions/base>

     # allow only the paths the container needs
     /app/** r,
     /tmp/** rw,
   }
   ```

3. **Pre-load** it into the target device's kernel (outside SSAM):

   ```console
   # apparmor_parser -r /etc/apparmor.d/ssam-prof-helloworld
   ```

4. Provide a `container-default` profile as the fallback for packages that ship without a dedicated one.
   Without either profile, a package with `mac = true` fails to start.

> ⚠️ Naming the profile in `runtime.json`'s `apparmorProfile` field has no effect: at runtime `ssamd` **overwrites that field** in the *OCI Runtime config* with the name derived from the package, so it is ignored.
> To turn MAC off entirely, set `mac = false`.
