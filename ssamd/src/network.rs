// Copyright 2026 Hyundai Mobis Co., Ltd.
// SPDX-License-Identifier: Apache-2.0

use std::collections::HashSet;

use anyhow::Context as _;
use netavark::network::types::PortMapping;
use rsa::sha2::{Digest, Sha256};

pub mod actor;
pub mod allocator;

pub use actor::{NetworkHandle, NetworkManager};

/// Default in-container network interface name when the package config omits
/// `[container.network] interface_name`.
pub const DEFAULT_CONTAINER_INTERFACE: &str = "eth0";

/// `ssam-` prefix plus a 10-hex-char id must fit within `IFNAMSIZ - 1` (15).
const _: () = assert!("ssam-".len() + 10 <= 15);

/// Single source of truth for the per-package id, shared by the veth host name
/// and the named netns so a package's interface and namespace stay in lockstep.
///
/// Returns the first 5 bytes of `SHA-256(pkg)` as 10 lowercase hex chars.
#[must_use]
pub(crate) fn pkg_hash10(pkg: &str) -> String {
    hex::encode(&Sha256::digest(pkg.as_bytes())[..5])
}

/// Host-side link helpers over `/sys/class/net` and netlink: existence checks
/// plus link deletion. Bridge creation itself is netavark's responsibility;
/// this module only queries and removes existing links.
pub mod link {
    use std::path::Path;

    use netavark::network::netlink::Socket;
    use netavark::network::netlink_route::{LinkID, NetlinkRoute};

    /// Sysfs root for network interfaces.
    const SYS_CLASS_NET: &str = "/sys/class/net";

    /// Whether a host link (bridge, veth, …) named `name` exists.
    #[must_use]
    pub fn exists(name: &str) -> bool {
        Path::new(SYS_CLASS_NET).join(name).exists()
    }

    /// Delete a host-side link (e.g. a veth) by name via netlink. Absent = success.
    ///
    /// Used to reclaim a stale or orphaned `ssam-*` veth whose container namespace
    /// is gone, where a full netavark teardown cannot run (it needs the netns).
    ///
    /// # Errors
    ///
    /// Returns an error if the host netlink socket cannot be opened or the delete
    /// request fails for a reason other than the link being absent.
    pub fn delete(name: &str) -> anyhow::Result<()> {
        // Serialized by the NetworkActor, so no TOCTOU between this check and delete.
        if !exists(name) {
            return Ok(());
        }
        let mut socket = Socket::<NetlinkRoute>::new()
            .map_err(|e| anyhow::anyhow!("Failed to open host netlink socket: {e}"))?;
        socket
            .del_link(LinkID::Name(name.to_owned()))
            .map_err(|e| anyhow::anyhow!("Failed to delete link {name}: {e}"))
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn exists_false_for_unknown_interface() {
            assert!(!exists("definitely-not-a-real-iface-xyz"));
        }

        #[test]
        fn delete_absent_is_ok() {
            assert!(delete("definitely-not-a-real-iface-xyz").is_ok());
        }
    }
}

pub mod netns {
    //! Named network namespace lifecycle, implemented purely via syscalls (no
    //! external `ip` command). A namespace is created by unsharing the network
    //! namespace of a dedicated thread and bind-mounting that thread's `net`
    //! namespace to a persistent path so it outlives the thread.
    //!
    //! The bind-mount target lives under the process temp directory
    //! ([`std::env::temp_dir`], i.e. `$TMPDIR`/`/tmp`) — the same writable,
    //! RAM-backed filesystem the daemon already requires for the OCI runtime
    //! bundle. Anchoring here (rather than `/run`) keeps the daemon's only
    //! writable-storage assumption to a single, non-negotiable location: a target
    //! that cannot run containers at all cannot provide this, so bridge networking
    //! imposes no new dependency. The namespace survives a daemon restart (the
    //! tmpfs persists) and resets on reboot (a fresh IP assignment is acceptable).

    use std::fs;
    use std::os::unix::fs::{DirBuilderExt, MetadataExt};
    use std::path::{Path, PathBuf};
    use std::thread;

    use anyhow::{Context, anyhow};
    use rustix::io::Errno;
    use rustix::mount::{UnmountFlags, mount_bind, unmount};
    use rustix::thread::{UnshareFlags, unshare_unsafe};

    /// Directory holding ssam named network namespaces, under the process temp
    /// directory (the writable tmpfs required for the OCI bundle).
    #[must_use]
    fn netns_dir() -> PathBuf {
        std::env::temp_dir().join("ssam").join("netns")
    }

    /// Named network namespace identifier for a package.
    ///
    /// Uses the identical suffix as the veth host interface so that a package's
    /// namespace and interface names stay in lockstep.
    #[must_use]
    pub fn netns_name(pkg: &str) -> String {
        format!("ssam-{}", crate::network::pkg_hash10(pkg))
    }

    /// Persistent path for a named network namespace.
    #[must_use]
    pub fn netns_path(name: &str) -> PathBuf {
        netns_dir().join(name)
    }

    /// Create `dir` as a root-only (0700) dir and reject it as tampering unless
    /// it is a real, root-owned, non-symlink directory with no group/other access.
    fn ensure_root_only_dir(dir: &Path) -> anyhow::Result<()> {
        match fs::DirBuilder::new().mode(0o700).create(dir) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => {
                return Err(e).with_context(|| {
                    format!("Failed to create netns directory {}", dir.display())
                });
            }
        }
        let meta = fs::symlink_metadata(dir)
            .with_context(|| format!("Failed to stat netns directory {}", dir.display()))?;
        anyhow::ensure!(
            meta.file_type().is_dir() && meta.uid() == 0 && meta.mode() & 0o777 == 0o700,
            "netns directory {} is not a root-only directory (possible tampering)",
            dir.display()
        );
        Ok(())
    }

    /// Create a persistent named network namespace at `path`.
    ///
    /// Idempotent: if `path` already exists this returns `Ok(())` without
    /// touching it, so a running namespace survives a daemon restart. Otherwise a
    /// dedicated thread unshares its network namespace and bind-mounts it onto the
    /// freshly created target file.
    ///
    /// # Errors
    ///
    /// Returns an error if the namespace directory or target file cannot be
    /// created, if unsharing or bind-mounting fails, or if the worker thread
    /// panics.
    // Safety: Debug format ({:?}) for paths instead of Display to prevent log
    // injection via special characters in netns paths.
    #[allow(clippy::use_debug, clippy::unnecessary_debug_formatting)]
    pub fn create_named_netns(path: &Path) -> anyhow::Result<()> {
        // SECURITY (TOCTOU): /tmp is world-writable, so validate EVERY ssam-owned
        // component, not just the leaf. An attacker who pre-creates an intermediate
        // dir they own could swap the checked leaf between stat and open. Top-down
        // validation closes this: a confirmed root-owned 0700 dir denies all
        // non-root access below it, and /tmp's sticky bit blocks renaming our entry.
        // Do NOT collapse this back to a single recursive create.
        let dir = netns_dir();
        let ssam_dir = dir.parent().context("netns directory has no parent")?;
        ensure_root_only_dir(ssam_dir)?;
        ensure_root_only_dir(&dir)?;

        // Existence is only trustworthy under the validated tree, so check it here,
        // not at function entry. Idempotent: a surviving namespace is preserved.
        if path.exists() {
            return Ok(());
        }

        // Create the empty bind-mount target.
        {
            let _file = fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(path)
                .with_context(|| format!("Failed to create netns target {}", path.display()))?;
        }

        let target = path.to_path_buf();
        let handle = thread::spawn(move || -> anyhow::Result<()> {
            // Move THIS thread into a fresh network namespace.
            // SAFETY: we unshare only `NEWNET`, never `FILES`, so the file
            // descriptor table is untouched and no fd-sharing invariants apply.
            unsafe { unshare_unsafe(UnshareFlags::NEWNET) }
                .context("Failed to unshare network namespace")?;
            // Pin the thread's net namespace to the persistent path so it
            // outlives the thread.
            mount_bind("/proc/thread-self/ns/net", &target).with_context(|| {
                format!("Failed to bind-mount net namespace to {}", target.display())
            })?;
            Ok(())
        });

        let outcome = match handle.join() {
            Ok(inner) => inner,
            Err(_) => Err(anyhow!("netns worker thread panicked")),
        };

        if outcome.is_err() {
            // The empty target was created before the bind mount. On failure it must
            // be removed, otherwise a later create is fooled by `path.exists()` into
            // reporting false success over a path with no namespace mounted.
            if let Err(e) = fs::remove_file(path) {
                log::warn!(
                    "Failed to remove stale netns target {path:?} after creation failure: {e}"
                );
            }
        }

        outcome
    }

    /// Delete a named network namespace at `path`.
    ///
    /// Idempotent: a missing or already-unmounted namespace is treated as
    /// success. The bind mount is detached lazily, then the target file removed.
    ///
    /// # Errors
    ///
    /// Returns an error only for unexpected unmount or removal failures (an absent
    /// target is not an error).
    pub fn delete_named_netns(path: &Path) -> anyhow::Result<()> {
        if let Err(e) = unmount(path, UnmountFlags::DETACH) {
            // Not mounted (EINVAL) or absent (ENOENT) are expected and ignored.
            if !matches!(e, Errno::INVAL | Errno::NOENT) {
                return Err(e)
                    .with_context(|| format!("Failed to unmount netns {}", path.display()));
            }
        }

        match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => {
                Err(e).with_context(|| format!("Failed to remove netns target {}", path.display()))
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use std::os::fd::OwnedFd;

        use super::*;

        fn open_netns_fd(path: &Path) -> anyhow::Result<OwnedFd> {
            let file = fs::File::open(path)
                .with_context(|| format!("Failed to open netns {}", path.display()))?;
            Ok(file.into())
        }

        #[test]
        fn netns_name_matches_veth_suffix() {
            let pkg = "foo";
            let ns = netns_name(pkg);
            let veth = crate::network::allocator::veth_host_name(pkg);
            assert_eq!(ns, veth, "netns and veth names must share the suffix");
            assert!(ns.starts_with("ssam-"));
        }

        #[test]
        fn netns_path_is_under_temp_dir() {
            let path = netns_path("ssam-abcdef0123");
            let expected = std::env::temp_dir()
                .join("ssam")
                .join("netns")
                .join("ssam-abcdef0123");
            assert_eq!(path, expected);
        }

        #[test]
        #[ignore = "requires root and CLONE_NEWNET; run under the Docker test harness"]
        fn create_and_delete_named_netns_roundtrip() {
            let name = "ssam-testns0001";
            let path = netns_path(name);
            // Best-effort cleanup from any prior failed run.
            let _ = delete_named_netns(&path);

            create_named_netns(&path).unwrap();
            assert!(path.exists());
            // Idempotent re-create must succeed without disturbing the namespace.
            create_named_netns(&path).unwrap();

            let fd = open_netns_fd(&path).unwrap();
            drop(fd);

            delete_named_netns(&path).unwrap();
            assert!(!path.exists());
            // Idempotent delete on an absent namespace is a no-op.
            delete_named_netns(&path).unwrap();
        }
    }
}

const MIN_PUBLISHABLE_HOST_PORT: u16 = 1024;

const PROTO_TCP: &str = "tcp";
const PROTO_UDP: &str = "udp";
const PROTO_SCTP: &str = "sctp";

/// Parse Docker-style `port_mappings` strings into netavark [`PortMapping`]s.
///
/// An empty or absent list yields `None` (not `Some(vec![])`).
pub(crate) fn parse_port_mappings(bindings: &[String]) -> anyhow::Result<Option<Vec<PortMapping>>> {
    if bindings.is_empty() {
        return Ok(None);
    }
    let mappings = bindings
        .iter()
        .map(|b| parse_port_mapping(b))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let mut seen = HashSet::new();
    for mapping in &mappings {
        anyhow::ensure!(
            seen.insert((mapping.protocol.clone(), mapping.host_port)),
            "duplicate host port {}/{} in port mappings",
            mapping.host_port,
            mapping.protocol
        );
    }
    Ok(Some(mappings))
}

/// Parse a single `HOST_PORT:CONTAINER_PORT[/PROTOCOL]` binding.
///
/// Protocol defaults to `tcp`; only lowercase `tcp`, `udp`, `sctp` are accepted.
fn parse_port_mapping(binding: &str) -> anyhow::Result<PortMapping> {
    let (ports, protocol) = binding.split_once('/').unwrap_or((binding, PROTO_TCP));
    let protocol = match protocol {
        PROTO_TCP | PROTO_UDP | PROTO_SCTP => protocol.to_owned(),
        other => anyhow::bail!("invalid protocol {other:?} in port mapping {binding:?}"),
    };
    let (host, container) = ports
        .split_once(':')
        .with_context(|| format!("missing ':' in port mapping {binding:?}"))?;
    let host_port: u16 = host
        .parse()
        .with_context(|| format!("invalid host port in port mapping {binding:?}"))?;
    let container_port: u16 = container
        .parse()
        .with_context(|| format!("invalid container port in port mapping {binding:?}"))?;
    anyhow::ensure!(
        host_port >= MIN_PUBLISHABLE_HOST_PORT && host_port != libssam::remocon::CONTROL_PORT,
        "host port {host_port} is reserved in port mapping {binding:?}"
    );
    anyhow::ensure!(
        container_port != 0,
        "container port 0 is invalid in port mapping {binding:?}"
    );
    Ok(PortMapping {
        host_port,
        container_port,
        host_ip: "0.0.0.0".to_owned(),
        protocol,
        range: 1,
    })
}

#[cfg(test)]
mod port_tests {
    use super::*;

    #[test]
    fn parse_port_mapping_defaults_to_tcp() {
        let m = parse_port_mapping("8080:80").expect("valid binding");
        assert_eq!(m.host_port, 8080);
        assert_eq!(m.container_port, 80);
        assert_eq!(m.protocol, "tcp");
        assert_eq!(m.host_ip, "0.0.0.0");
        assert_eq!(m.range, 1);
    }

    #[test]
    fn parse_port_mapping_explicit_udp() {
        let m = parse_port_mapping("5353:53/udp").expect("valid udp binding");
        assert_eq!(m.host_port, 5353);
        assert_eq!(m.container_port, 53);
        assert_eq!(m.protocol, "udp");
    }

    #[test]
    fn parse_port_mapping_explicit_sctp() {
        let m = parse_port_mapping("9899:9899/sctp").expect("valid sctp binding");
        assert_eq!(m.host_port, 9899);
        assert_eq!(m.container_port, 9899);
        assert_eq!(m.protocol, "sctp");
    }

    #[test]
    fn parse_port_mappings_empty_is_none() {
        assert!(parse_port_mappings(&[]).expect("empty ok").is_none());
    }

    #[test]
    fn parse_port_mappings_rejects_duplicate_host_protocol() {
        let bindings = ["8080:80".to_owned(), "8080:81/tcp".to_owned()];
        assert!(parse_port_mappings(&bindings).is_err());
    }

    #[test]
    fn parse_port_mapping_rejects_malformed() {
        for bad in [
            "8080",
            "8080:",
            ":80",
            "8080:80:90",
            "70000:80",
            "8080:70000",
            "8080:80/icmp",
            "8080:80/tcp,udp",
            "8080:80/TCP",
            "8080:80/",
            "0:80",
            "8080:0",
            "22:22",
            "443:443",
            "63737:80",
        ] {
            assert!(
                parse_port_mapping(bad).is_err(),
                "expected {bad:?} to be rejected"
            );
        }
    }
}
