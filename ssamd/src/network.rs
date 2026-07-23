// Copyright 2026 Hyundai Mobis Co., Ltd.
// SPDX-License-Identifier: Apache-2.0

use rsa::sha2::{Digest, Sha256};

pub mod actor;
pub mod allocator;

pub use actor::NetworkManager;

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

/// Host bridge interface name for a bridge id (`network_name` or package name).
///
/// `ssb-<hash10>`. The `ssb-` prefix (vs. `ssam-` for veth/netns) both marks it
/// as a bridge in a sysfs scan and is what the netavark firewall hash rebuilds
/// from — keep it stable.
#[must_use]
pub fn bridge_name(bridge_id: &str) -> String {
    format!("ssb-{}", pkg_hash10(bridge_id))
}

/// Host-side link helpers over `/sys/class/net` and netlink: existence checks,
/// master/address lookups, and link deletion. Bridge creation itself is
/// netavark's responsibility; this module only queries and removes existing links.
pub mod link {
    use std::path::Path;

    use netavark::network::netlink::Socket;
    use netavark::network::netlink_route::{LinkID, NetlinkRoute};
    use netlink_packet_route::link::LinkAttribute;

    /// Sysfs root for network interfaces.
    pub(crate) const SYS_CLASS_NET: &str = "/sys/class/net";

    /// Whether a host link (bridge, veth, …) named `name` exists.
    #[must_use]
    pub fn exists(name: &str) -> bool {
        Path::new(SYS_CLASS_NET).join(name).exists()
    }

    /// Open a netlink route socket to the host network namespace.
    fn open_route_socket() -> anyhow::Result<Socket<NetlinkRoute>> {
        Socket::<NetlinkRoute>::new()
            .map_err(|e| anyhow::anyhow!("Failed to open host netlink socket: {e}"))
    }

    /// The bridge that `veth` is a port of (its netlink master), or `None` if
    /// `veth` is not enslaved to any bridge.
    ///
    /// The kernel stores the master as an ifindex (`IFLA_MASTER`, the
    /// "controller"), so this reads the index then looks up its name in a second
    /// query.
    ///
    /// # Errors
    ///
    /// Fails if the netlink socket cannot be opened or a link query fails. A veth
    /// with no master is `Ok(None)`, not an error.
    pub fn enslaving_bridge(veth: &str) -> anyhow::Result<Option<String>> {
        let mut socket = open_route_socket()?;
        let link = socket
            .get_link(LinkID::Name(veth.to_owned()))
            .map_err(|e| anyhow::anyhow!("Failed to get link {veth}: {e}"))?;

        let controller = link.attributes.iter().find_map(|attr| match attr {
            LinkAttribute::Controller(index) => Some(*index),
            _ => None,
        });

        let Some(index) = controller else {
            return Ok(None);
        };

        let master = socket
            .get_link(LinkID::ID(index))
            .map_err(|e| anyhow::anyhow!("Failed to get master link {index}: {e}"))?;
        Ok(master.attributes.into_iter().find_map(|attr| match attr {
            LinkAttribute::IfName(name) => Some(name),
            _ => None,
        }))
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
        let mut socket = open_route_socket()?;
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
    use rustix::fs::{Mode, OFlags, open};
    use rustix::io::Errno;
    use rustix::ioctl::{Ioctl, IoctlOutput, Opcode, ioctl as rustix_ioctl};
    use rustix::mount::{UnmountFlags, mount_bind, unmount};
    use rustix::thread::{UnshareFlags, unshare_unsafe};

    /// Directory holding ssam named network namespaces, under the process temp
    /// directory (the writable tmpfs required for the OCI bundle).
    #[must_use]
    pub(crate) fn netns_dir() -> PathBuf {
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

    /// Validate the ssam-owned netns tree (`netns_dir()` and its parent) is a
    /// real, root-owned, non-symlink, 0700 directory before any caller reads or
    /// `setns`es beneath it. Callers share this `/tmp`-rooted tree, so without
    /// the check an attacker could pre-create `netns_dir()` and smuggle in a
    /// trusted symlink.
    ///
    /// # Errors
    ///
    /// Errors if either directory cannot be created/read, or is not euid-owned
    /// with mode exactly 0700.
    pub(crate) fn ensure_netns_tree_trusted() -> anyhow::Result<()> {
        let dir = netns_dir();
        let ssam_dir = dir.parent().context("netns directory has no parent")?;
        ensure_root_only_dir(ssam_dir)?;
        ensure_root_only_dir(&dir)
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
        // The netns tree is created and trust-validated once at daemon startup
        // (`NetworkManager::new`). Under sticky /tmp a confirmed root-0700 tree
        // cannot then be tampered by non-root, so callers trust it here without
        // re-checking; existence below is only meaningful under that trusted tree.
        // Idempotent: a real surviving netns is preserved across restart. A bare
        // target left by a crash mid-create is not a netns — clear it and recreate,
        // else a later setns on the plain file fails forever.
        if path.exists() {
            if verify_is_netns(path).is_ok() {
                return Ok(());
            }
            delete_named_netns(path).with_context(|| {
                format!("Failed to clear stale netns target {}", path.display())
            })?;
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
        // Not mounted (EINVAL) or absent (ENOENT) are expected and ignored.
        if let Err(e) = unmount(path, UnmountFlags::DETACH)
            && !matches!(e, Errno::INVAL | Errno::NOENT)
        {
            return Err(e).with_context(|| format!("Failed to unmount netns {}", path.display()));
        }

        match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => {
                Err(e).with_context(|| format!("Failed to remove netns target {}", path.display()))
            }
        }
    }

    /// `NS_GET_NSTYPE` (`linux/nsfs.h`, `_IO(0xb7, 3)`) takes no data; the
    /// namespace type is the ioctl's own return value rather than something
    /// written through a pointer, so `output_from_ptr` reads `out` instead of
    /// the (null) argument pointer.
    struct NsGetNsType;

    unsafe impl Ioctl for NsGetNsType {
        type Output = i32;

        const IS_MUTATING: bool = false;

        fn opcode(&self) -> Opcode {
            linux_raw_sys::ioctl::NS_GET_NSTYPE
        }

        fn as_ptr(&mut self) -> *mut rustix::ffi::c_void {
            std::ptr::null_mut()
        }

        unsafe fn output_from_ptr(
            out: IoctlOutput,
            _extract_output: *mut rustix::ffi::c_void,
        ) -> rustix::io::Result<Self::Output> {
            Ok(out)
        }
    }

    /// Verify `path` is an existing network namespace (`NS_GET_NSTYPE` == `CLONE_NEWNET`).
    /// Used for the external-netns container mode: the sysadmin provisions the netns;
    /// this only fails fast+clean on a wrong/missing path rather than deep in crun.
    ///
    /// # Errors
    ///
    /// Errors if the path cannot be opened or is not a network namespace.
    // Safety: Debug format ({:?}) for paths instead of Display to prevent log
    // injection via special characters in netns paths.
    #[allow(clippy::use_debug, clippy::unnecessary_debug_formatting)]
    pub fn verify_is_netns(path: &Path) -> anyhow::Result<()> {
        // NONBLOCK: a package-controlled path could be a writer-less FIFO whose
        // O_RDONLY open would block this thread forever.
        let fd = open(
            path,
            OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NONBLOCK,
            Mode::empty(),
        )
        .with_context(|| format!("Failed to open {path:?}"))?;
        // SAFETY: NS_GET_NSTYPE takes no arguments and only reads namespace type
        // metadata off `fd`; it does not mutate userspace memory.
        let ns_type = unsafe { rustix_ioctl(fd, NsGetNsType) }
            .with_context(|| format!("NS_GET_NSTYPE ioctl failed on {path:?}"))?;
        anyhow::ensure!(
            ns_type == linux_raw_sys::general::CLONE_NEWNET.cast_signed(),
            "{path:?} is not a network namespace (NS_GET_NSTYPE returned {ns_type:#x})"
        );
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use std::os::fd::OwnedFd;
        use std::os::unix::fs::PermissionsExt;

        use super::*;

        fn open_netns_fd(path: &Path) -> anyhow::Result<OwnedFd> {
            let file = fs::File::open(path)
                .with_context(|| format!("Failed to open netns {}", path.display()))?;
            Ok(file.into())
        }

        #[test]
        #[ignore = "requires root; ensure_root_only_dir checks meta.uid() == 0"]
        fn ensure_root_only_dir_creates_and_accepts_0700() {
            let parent = tempfile::TempDir::new().expect("tempdir");
            let dir = parent.path().join("fresh");
            ensure_root_only_dir(&dir).expect("first call creates a 0700 dir");
            // Idempotent: an existing dir that already satisfies the check passes again.
            ensure_root_only_dir(&dir).expect("second call accepts the same trusted dir");
        }

        #[test]
        #[ignore = "requires root; ensure_root_only_dir checks meta.uid() == 0"]
        fn ensure_root_only_dir_rejects_group_or_other_writable() {
            let parent = tempfile::TempDir::new().expect("tempdir");
            let dir = parent.path().join("loose");
            fs::create_dir(&dir).expect("create dir");
            fs::set_permissions(&dir, fs::Permissions::from_mode(0o770)).expect("chmod 0770");
            let err = ensure_root_only_dir(&dir).expect_err("0770 must be rejected as tampering");
            assert!(
                format!("{err:#}").contains("not a root-only directory"),
                "unexpected error: {err:#}"
            );
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
        fn bridge_name_is_stable_sized_and_distinct() {
            let name = crate::network::bridge_name("foo");
            assert_eq!(name, crate::network::bridge_name("foo"));
            assert_eq!(name.len(), 14);
            assert!(name.starts_with("ssb-"));
            assert_ne!(name, crate::network::allocator::veth_host_name("foo"));
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

            // create_named_netns no longer establishes the tree itself; the daemon
            // does that once via NetworkManager::new. Mirror that here.
            ensure_netns_tree_trusted().unwrap();
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

            // A stale bare target (crash mid-create) is not a netns: create must
            // clear and recreate it rather than trust bare existence.
            fs::File::create(&path).unwrap();
            assert!(verify_is_netns(&path).is_err());
            create_named_netns(&path).unwrap();
            verify_is_netns(&path).expect("stale target must be recreated as a real netns");
            delete_named_netns(&path).unwrap();
        }

        #[test]
        fn verify_is_netns_rejects_non_netns_path() {
            // /proc/self/ns/mnt supports NS_GET_NSTYPE (it is a namespace fd) but
            // reports CLONE_NEWNS, not CLONE_NEWNET, so this exercises the real
            // "wrong namespace type" rejection without requiring root.
            let path = Path::new("/proc/self/ns/mnt");
            let err =
                verify_is_netns(path).expect_err("a mount namespace is not a network namespace");
            assert!(
                format!("{err:#}").contains("not a network namespace"),
                "unexpected error: {err:#}"
            );
        }

        #[test]
        #[ignore = "requires root and CLONE_NEWNET; run under the Docker test harness"]
        fn verify_is_netns_accepts_real_netns() {
            let path = netns_path("ssam-verify-test");
            let _ = delete_named_netns(&path);
            ensure_netns_tree_trusted().unwrap();
            create_named_netns(&path).unwrap();

            verify_is_netns(&path).expect("a real netns must pass verification");

            delete_named_netns(&path).unwrap();
        }
    }
}
