# SSAM Package Integrity

<p>
  <a href="package-integrity.ko.md"><img src="https://img.shields.io/badge/lang-한국어-blue" alt="Korean"></a>
</p>

SSAM guarantees that a device runs **only trusted, unmodified** packages and
keeps detecting tampering for as long as they run. It does this with an
**integrity model** built from three parts: a trust anchor (the on-device public
key), an RSA signature, and dm-verity.

---

## Table of Contents

1. [Trust model](#trust-model)
2. [Integrity chain overview](#integrity-chain-overview)
3. [Package authenticity — RSA signature](#package-authenticity--rsa-signature)
4. [Filesystem integrity — dm-verity](#filesystem-integrity--dm-verity)
5. [Hardening the package parser](#hardening-the-package-parser)
6. [Control interface security (RPC)](#control-interface-security-rpc)
7. [Threat model summary](#threat-model-summary)

---

## Trust model

The root of trust is an **RSA key pair**:

- The **private key** lives off-device, with whoever builds packages;
  `ssam-wrap` signs each package with it at build time.
- The **public key** is installed on the device and is the only thing `ssamd`
  trusts. Any package it cannot verify against that key is rejected.

Two things must therefore be protected:

- **The private key's secrecy** — anyone holding it can produce packages the
  device will accept.
- **The on-device public key's integrity** — it is the trust anchor. If an
  attacker can replace it, they can substitute their own signing key, so it must
  be protected with the platform's normal mechanisms (read-only rootfs, secure
  boot, file permissions).

`ssamd` itself runs as **root**, so a package that passes verification runs with
high privilege — which is exactly why authenticity is enforced before anything
else runs.

---

## Integrity chain overview

SSAM protects a package in two stages. A single few-kilobyte check at load time
extends into continuous protection of the whole filesystem for as long as the
package runs:

```mermaid
flowchart LR
  subgraph SIGN["Build — sign (off-device)"]
    direction TB
    IMG["package filesystem image<br/>(large, unsigned)"]
    IMG -->|"build verity table"| RH["dm-verity table<br/>root hash + params"]
    RH -->|"stored in"| CFG["metadata · runtime_config · seccomp<br/>(small)"]
    CFG -->|"RSA sign (private key)"| SIG["signature"]
  end

  subgraph DEV["On-device — verify (ssamd)"]
    direction TB
    ONCE["① load: verify signature<br/>(public key)"]
    CONT["② every read: dm-verity checks<br/>each block vs root hash — continuous"]
    ONCE -->|"root hash now trusted"| CONT
  end

  SIGN ==>|".ssam package"| DEV
```

- **Sign (build).** `ssam-wrap` RSA-signs only the **configuration data** — the
  package filesystem image is not signed.
- **Verify (load).** `ssamd` verifies the signature at load with the public key.
- **Verify continuously (runtime).** The kernel's dm-verity re-checks each
  filesystem block **on every read**.

The signature covers only a few kilobytes but, through the embedded root hash,
**transitively protects the entire filesystem**.

This split is a deliberate **performance choice**. Signing the whole filesystem
would force re-verifying all of it at load, slowing startup; instead SSAM signs
only the few-kilobyte configuration data and leaves the filesystem to dm-verity,
which verifies it on demand at runtime. The load-time signature check therefore
stays cheap — keeping container startup fast while still guaranteeing integrity at
runtime.

The next two sections detail each half.

---

## Package authenticity — RSA signature

Each `.ssam` package carries an **RSA PKCS#1 v1.5 signature over a SHA-256
digest** of its **configuration data** — the few kilobytes that describe how the
package is built and run.

- Signing uses the private key; `ssamd` verifies with the public key **before
  parsing any of those payloads**, deciding whether to trust the package.
- A package signed by a different key — or with any signed payload altered —
  fails verification and is refused.

The signature deliberately does **not** cover the large package filesystem image or the
signature payload itself — the filesystem image is protected indirectly (next
section).

---

## Filesystem integrity — dm-verity

The package filesystem image is protected by **dm-verity**:

- dm-verity implements the runtime package filesystem integrity policy. The
  policy is enabled by default and can be disabled through
  `[integrity].disable_runtime_integrity`. Package signature verification
  remains enabled when this policy is disabled.
- The signed `metadata` contains the **entire set of dm-verity parameters**
  (including the root hash).
- At load time `ssamd` builds the dm-verity target from those signed parameters.
  By dm-verity's design, **every block is re-verified on each read**, so
  tampering is detected not just at load but **continuously at runtime**.

---

## Hardening the package parser

`ssamd` cannot trust a package's contents until its signature is verified, so it
bounds what a malicious file can do **before** verification completes:

- **Verify-then-parse ordering.** The RSA signature is checked first; the
  configuration payloads are deserialized and trusted **only after**
  verification succeeds.
- **Bounded pre-auth allocation.** Both the payload index decode and each signed
  payload read during verification are capped at small fixed sizes, so an
  attacker-controlled size field cannot force unbounded allocation.
- **Offset/size bounds checks.** Every declared payload region is validated
  against the actual file size before reading, rejecting out-of-range offsets.
- **Format gate.** A missing/incorrect magic or an unrecognized format version
  is rejected before any payload processing.

---

## Control interface security (RPC)

- `ssamd` exposes the [Remocon gRPC control API](../reference/remocon-rpc.md) on
  port **63737**, bound to [`rpc_bind_ip`](../reference/daemon-config.md#rpc_bind_ip)
  (localhost by default). There is no TLS or authentication, so any client that
  can reach the endpoint can install a package.
- Nonetheless, **authenticity is still enforced regardless of who calls the
  RPC:** an attacker may trigger installation, but a package that fails
  signature verification is refused at the install step and never gets set up
  or run. Unsigned or tampered code therefore cannot actually execute.

---

## Threat model summary

This table covers the threats to **package integrity and authenticity**. For
container-level threats (syscalls, host access, storage, network), see
[Container Isolation](container-isolation.md).

| Threat | Primary mitigation |
| --- | --- |
| Forged / untrusted package | RSA signature verified against on-device public key |
| Tampered configuration data | Included in the signed digest |
| Tampered package filesystem image | dm-verity root hash inside signed metadata; continuous block verification |
| Runtime tampering / corruption of the package filesystem | dm-verity verifies every block at the block-device level on read; read-only mount |
| Malicious package file (parser DoS) | Verify-then-parse, decode/size caps, bounds checks |

---

For container-level confinement (seccomp, AppArmor, resource limits, networking),
see [Container Isolation](container-isolation.md). For the byte-level details of
signing and verification, see the
[Package Format Reference](package-format.md#signature-and-integrity).
