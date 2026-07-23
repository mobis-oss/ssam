// Copyright 2026 Hyundai Mobis Co., Ltd.
// SPDX-License-Identifier: Apache-2.0

//! `NetworkActor` and its [`NetworkManager`] wrapper: the rsactor front-end that
//! serializes per-bridge network state and drives the netavark 2.0
//! bridge/veth/NAT integration.
//!
//! The actor carves per-container bridges out of a shared `pool` supernet: a
//! [`SubnetAllocator`] hands each bridge a distinct `/subnet_prefix`, and each
//! live bridge is a [`BridgeIpAllocator`] over its own subnet (empty = no
//! attachments). Several containers may share one bridge by resolving to the same
//! bridge id; the bridge is created on first attach and destroyed on last detach.
//! It exposes four idempotent operations:
//! - [`CreateNetns`](messages::CreateNetns): create the persistent named
//!   network namespace (never destroys an existing one).
//! - [`Attach`](messages::Attach): run netavark `setup` to wire veth + IP +
//!   NAT into the namespace, creating the bridge on first attach.
//! - [`Detach`](messages::Detach): delete the host veth and release the IP; on
//!   last detach the empty bridge is torn down.
//! - [`DestroyNetns`](messages::DestroyNetns): delete the named namespace.
//!
//! Only the netavark bridge `setup` (blocking netlink I/O) runs on
//! [`tokio::task::spawn_blocking`]; the remaining short netlink/mount syscalls
//! run inline. Either way each handler is serialized through this single actor
//! — the property that keeps the allocator/refcount state race-free.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::net::Ipv4Addr;
use std::path::Path;

use anyhow::Context as _;
use ipnet::Ipv4Net;
use rsactor::{Actor, ActorRef, message_handlers};

use crate::network::allocator::{self, BridgeIpAllocator, SubnetAllocator};
use crate::network::{link, netns};
use crate::utils::actor_supervisor::{IgnoreOnFailure, SupervisedActor, spawn_with};

mod netavark_ops;
mod restore;

pub(crate) mod messages {
    //! Plain message structs handled by [`super::NetworkActor`].

    /// Create the persistent named network namespace for `pkg`.
    pub(crate) struct CreateNetns {
        pub pkg: String,
    }

    /// Wire veth + IP + NAT into `pkg`'s namespace on the bridge resolved from
    /// `bridge_id` (`network_name` or package name), exposing the resolved
    /// container-side interface as `container_interface`.
    pub(crate) struct Attach {
        pub pkg: String,
        pub bridge_id: String,
        pub container_interface: String,
    }

    /// Detach `pkg` from the bridge resolved from `bridge_id`: on last detach the
    /// host veth is deleted and, once empty, the bridge is torn down.
    pub(crate) struct Detach {
        pub pkg: String,
        pub bridge_id: String,
    }

    /// Delete `pkg`'s named network namespace.
    pub(crate) struct DestroyNetns {
        pub pkg: String,
    }
}

/// Owns every live bridge's subnet/IP/refcount state.
#[derive(Debug)]
pub struct NetworkActor {
    subnets: SubnetAllocator,
    /// Live bridges keyed by `ssb-<hash>` interface name; each value is that
    /// bridge's IP allocator over its subnet (empty = no attachments).
    bridges: HashMap<String, BridgeIpAllocator>,
}

impl Actor for NetworkActor {
    type Args = Self;
    type Error = std::convert::Infallible;

    /// Rebuild bridge/subnet/IP state from host truth so a container that
    /// outlived the daemon keeps its exact address.
    async fn on_start(mut actor: Self, _actor_ref: &ActorRef<Self>) -> Result<Self, Self::Error> {
        actor.restore_from_host().await;
        Ok(actor)
    }
}

impl SupervisedActor for NetworkActor {
    type FailurePolicy = IgnoreOnFailure;
}

#[message_handlers]
impl NetworkActor {
    /// Build an actor from bridge network configuration. `[network.bridge]` is
    /// already validated at config load (`BridgeConfig::validate`).
    ///
    /// # Errors
    ///
    /// Returns an error if `config.base` is not a valid IPv4 CIDR or the pool is
    /// too coarse to hold one `/subnet_prefix` subnet.
    pub fn new(config: &crate::configuration::BridgeConfig) -> anyhow::Result<Self> {
        let pool: Ipv4Net = config
            .base()
            .parse()
            .with_context(|| format!("Invalid network base {:?}", config.base()))?;
        let subnets = SubnetAllocator::new(pool, config.size())?;
        Ok(Self {
            subnets,
            bridges: HashMap::new(),
        })
    }

    /// Idempotently create the package's persistent named namespace.
    ///
    /// An existing namespace is preserved (never destroyed) so a running
    /// container survives a daemon restart.
    #[handler]
    // rsactor #[handler] requires async fn signature even without await
    #[allow(clippy::unused_async)]
    async fn handle_create_netns(
        &mut self,
        msg: messages::CreateNetns,
        _actor_ref: &ActorRef<Self>,
    ) -> anyhow::Result<()> {
        let name = netns::netns_name(&msg.pkg);
        let path = netns::netns_path(&name);
        netns::create_named_netns(&path)
    }

    /// Idempotently attach veth + IP + NAT for the package onto its bridge.
    ///
    /// Creates the bridge (allocating a distinct subnet from the pool) on first
    /// attach. If the host veth and the namespace are both already present the
    /// existing wiring is reused without re-running netavark or re-counting the
    /// refcount. A veth left over from a destroyed namespace is removed and
    /// recreated.
    #[handler]
    async fn handle_attach(
        &mut self,
        msg: messages::Attach,
        _actor_ref: &ActorRef<Self>,
    ) -> anyhow::Result<()> {
        // netns tree already validated at startup by `NetworkManager::new`.
        let messages::Attach {
            pkg,
            bridge_id,
            container_interface,
        } = msg;
        netavark_ops::validate_interface_name(&container_interface).with_context(|| {
            format!("Invalid container network interface_name {container_interface:?}")
        })?;
        let veth = allocator::veth_host_name(&pkg);
        let br = crate::network::bridge_name(&bridge_id);
        let ns_name = netns::netns_name(&pkg);
        let ns_path = netns::netns_path(&ns_name);

        if self.adopt_existing(&br, &veth, &ns_path)? {
            return Ok(());
        }
        if let Err(e) = link::delete(&veth) {
            log::warn!("Failed to remove stale veth {veth} before re-setup: {e}");
        }

        // Resolve the netns path before allocating: a non-UTF8 path must not strand
        // an allocated IP/subnet (the allocation is rolled back only below).
        let netns_path = ns_path
            .to_str()
            .context("netns path is not valid UTF-8")?
            .to_owned();

        let (subnet, ip) = self.allocate_container_ip(&br, &veth)?;
        let options =
            netavark_ops::build_network_options(&pkg, &br, subnet, ip, &container_interface);

        let setup =
            tokio::task::spawn_blocking(move || netavark_ops::run_setup(&netns_path, &options))
                .await;

        // Roll back on any failure — a setup error OR a panic in netavark (surfaced
        // as a JoinError). Skipping rollback on panic would leak the IP/subnet and
        // leave a half-wired veth a later attach could adopt as if it were live.
        let setup_err = match setup {
            Ok(Ok(())) => None,
            Ok(Err(e)) => Some(e),
            Err(join) => Some(anyhow::Error::new(join).context("netavark setup task panicked")),
        };
        if let Some(e) = setup_err {
            // Delete the half-created veth before rollback: else the bridge's
            // slave-check still sees it and leaks the now-empty bridge.
            if let Err(del) = link::delete(&veth) {
                log::warn!("Failed to remove veth {veth} after netavark setup failure: {del}");
            }
            self.rollback_attach(&br, &veth);
            return Err(e);
        }

        Ok(())
    }

    /// Whether a live veth+netns can be adopted as-is. `Ok(false)` means there is
    /// nothing to adopt (proceed to a fresh setup).
    ///
    /// # Errors
    ///
    /// Fails closed if the veth+netns are live but the actor has no tracked
    /// bridge or address for them (refuse to trust a half-known attachment).
    fn adopt_existing(&self, br: &str, veth: &str, ns_path: &Path) -> anyhow::Result<bool> {
        if !(link::exists(veth) && ns_path.exists()) {
            return Ok(false);
        }
        let state = self.bridges.get(br).with_context(|| {
            format!(
                "veth {veth} and its netns exist but bridge {br} is untracked; refusing to allocate"
            )
        })?;
        state.ip_for(veth).with_context(|| {
            format!("veth {veth} and its netns exist but no tracked address; refusing to allocate")
        })?;
        Ok(true)
    }

    /// Ensure the bridge exists (allocating a pool subnet on first attach) and
    /// allocate a container IP for `veth`.
    ///
    /// # Errors
    ///
    /// Returns an error if the pool has no free subnet or the bridge subnet is
    /// exhausted.
    fn allocate_container_ip(
        &mut self,
        br: &str,
        veth: &str,
    ) -> anyhow::Result<(Ipv4Net, Ipv4Addr)> {
        let created = !self.bridges.contains_key(br);
        let state = match self.bridges.entry(br.to_owned()) {
            Entry::Occupied(e) => e.into_mut(),
            Entry::Vacant(e) => {
                let subnet = self
                    .subnets
                    .allocate(br)
                    .with_context(|| format!("allocate subnet for bridge {br}"))?;
                e.insert(BridgeIpAllocator::new(subnet))
            }
        };
        match state.allocate(veth) {
            Ok(ip) => Ok((state.subnet(), ip)),
            Err(e) => {
                // Undo a first-attach bridge creation so its subnet isn't stranded.
                if created {
                    self.destroy_bridge(br);
                }
                Err(e).with_context(|| format!("allocate IP on bridge {br}"))
            }
        }
    }

    /// Idempotently detach the package from its bridge, keeping its namespace.
    /// Best-effort and forgiving: always returns `Ok`.
    ///
    /// Deletes the host veth and releases its IP. On last detach (the bridge's
    /// IP set becomes empty) the bridge link is removed and its subnet released
    /// back to the pool.
    #[handler]
    // rsactor #[handler] requires async fn signature even without await
    #[allow(clippy::unused_async)]
    async fn handle_detach(
        &mut self,
        msg: messages::Detach,
        _actor_ref: &ActorRef<Self>,
    ) -> anyhow::Result<()> {
        let messages::Detach { pkg, bridge_id } = msg;
        let veth = allocator::veth_host_name(&pkg);
        let br = crate::network::bridge_name(&bridge_id);

        // On delete failure keep the IP mark: reusing it would collide with the
        // surviving veth.
        match link::delete(&veth) {
            Ok(()) => {
                if let Some(state) = self.bridges.get_mut(&br) {
                    state.release(&veth);
                    if state.is_empty() {
                        self.destroy_bridge(&br);
                    }
                }
            }
            Err(e) => {
                log::warn!("Best-effort veth teardown for {pkg} failed; keeping IP marked: {e}");
            }
        }
        Ok(())
    }

    /// Idempotently delete the package's named namespace. Absent = success.
    #[handler]
    // rsactor #[handler] requires async fn signature even without await
    #[allow(clippy::unused_async)]
    async fn handle_destroy_netns(
        &mut self,
        msg: messages::DestroyNetns,
        _actor_ref: &ActorRef<Self>,
    ) -> anyhow::Result<()> {
        let name = netns::netns_name(&msg.pkg);
        let path = netns::netns_path(&name);
        netns::delete_named_netns(&path)
    }

    /// Roll back a failed fresh attach: release the IP and tear down the bridge
    /// if it is now empty.
    fn rollback_attach(&mut self, br: &str, veth: &str) {
        let should_destroy = match self.bridges.get_mut(br) {
            Some(state) => {
                state.release(veth);
                state.is_empty()
            }
            None => return,
        };
        if should_destroy {
            self.destroy_bridge(br);
        }
    }

    /// Remove a now-unused bridge — but only once the host bridge is slave-less.
    /// While a real `ssam-*` veth is still enslaved (refcount diverged from host),
    /// keep the subnet claimed: freeing it could reissue a live bridge's subnet.
    fn destroy_bridge(&mut self, br: &str) {
        if bridge_has_managed_slaves(br) {
            log::warn!(
                "bridge {br} still has a managed slave; keeping its subnet claimed (refcount diverged from host)"
            );
            return;
        }
        self.subnets.release(br);
        self.bridges.remove(br);
        if let Err(e) = link::delete(br) {
            log::warn!("Failed to remove empty bridge {br}: {e}");
        }
    }

    /// Rebuild bridge/subnet/IP state from host truth, then sweep orphans.
    /// Best-effort: a failure skips one attachment and is logged, never aborts start.
    pub(super) async fn restore_from_host(&mut self) {
        let pool = self.subnets.pool();
        let subnet_prefix = self.subnets.subnet_prefix();
        // setns is not unwind-safe, so run the scan on a throwaway OS thread — a
        // panic discards the thread, not a tokio worker. A failed scan yields an
        // empty plan but still falls through to the orphan sweep.
        let plan = match tokio::task::spawn_blocking(move || {
            std::thread::spawn(move || restore::collect_restore_plan(pool, subnet_prefix)).join()
        })
        .await
        {
            Ok(Ok(plan)) => plan,
            Ok(Err(_)) => {
                log::warn!("network restore scan thread panicked; skipping bridge restore");
                Vec::new()
            }
            Err(e) => {
                log::warn!("network restore scan task failed to join: {e}");
                Vec::new()
            }
        };
        self.apply_restore_plan(plan);
        Self::cleanup_orphans();
    }

    /// Apply the restore plan: claim each restored bridge's subnet + live IP,
    /// then reserve slots for stragglers. Attaches run first.
    fn apply_restore_plan(&mut self, plan: Vec<restore::RestoreEntry>) {
        let mut reserves = Vec::new();
        for entry in plan {
            match entry {
                restore::RestoreEntry::Attach(att) => self.claim_restored_attach(att),
                restore::RestoreEntry::ReserveIp { ip, veth, master } => {
                    reserves.push((ip, veth, master));
                }
            }
        }
        for (ip, veth, master) in reserves {
            self.reserve_ip(ip, &veth, master.as_deref());
        }
    }

    /// Reserve `ip` under its real veth id in the known sibling bridge, if
    /// restored; else fall back to a pool-level-only reservation.
    fn reserve_ip(&mut self, ip: Ipv4Addr, veth: &str, master: Option<&str>) {
        if let Some(br) = master
            && let Some(alloc) = self.bridges.get_mut(br)
        {
            if let Err(e) = alloc.claim(veth, ip) {
                log::warn!("network restore: reserve ip {ip} for veth {veth} on {br}: {e}");
            }
            return;
        }
        self.subnets.reserve_containing(ip);
    }

    /// Claim a fully-restored attachment: its bridge subnet and live container IP.
    fn claim_restored_attach(&mut self, att: restore::RestoredAttach) {
        let restore::RestoredAttach {
            br,
            subnet,
            veth,
            ip,
        } = att;
        if let Err(e) = self.subnets.claim(&br, subnet) {
            log::warn!("network restore: claim subnet {subnet} for bridge {br}: {e}");
            return;
        }
        let state = self
            .bridges
            .entry(br)
            .or_insert_with(|| BridgeIpAllocator::new(subnet));
        if let Err(e) = state.claim(&veth, ip) {
            log::warn!("network restore: claim ip {ip} for veth {veth}: {e}");
        }
    }

    /// Delete orphaned `ssam-*` veths (netns gone), then slave-less `ssb-*`
    /// bridges — veths first so a bridge only looks slave-less once cleared.
    fn cleanup_orphans() {
        let (veths, bridges) = restore::enumerate_managed_links();
        for veth in veths.into_iter().filter(|v| !netns::netns_path(v).exists()) {
            if let Err(e) = link::delete(&veth) {
                log::warn!("network restore: orphan veth {veth} delete failed: {e}");
            }
        }
        for br in &bridges {
            if !bridge_has_managed_slaves(br)
                && let Err(e) = link::delete(br)
            {
                log::warn!("network restore: orphan bridge {br} delete failed: {e}");
            }
        }
    }
}

/// Whether bridge `br` still has an enslaved `ssam-*` veth, read from
/// `/sys/class/net/<br>/brif`. A missing directory counts as no slaves.
fn bridge_has_managed_slaves(br: &str) -> bool {
    let brif = Path::new(link::SYS_CLASS_NET).join(br).join("brif");
    match std::fs::read_dir(brif) {
        Ok(entries) => entries.flatten().any(|e| {
            e.file_name()
                .to_str()
                .is_some_and(|n| n.starts_with("ssam-"))
        }),
        Err(_) => false,
    }
}

/// Async wrapper over a spawned [`NetworkActor`].
#[derive(Debug, Clone)]
pub struct NetworkManager {
    actor: ActorRef<NetworkActor>,
}

impl NetworkManager {
    /// Build a [`NetworkActor`] from `config` and spawn it under supervision.
    ///
    /// # Errors
    ///
    /// Returns an error if the actor cannot be constructed (invalid pool /
    /// `subnet_prefix`) or the netns tree fails its trust check.
    pub fn new(config: &crate::configuration::BridgeConfig) -> anyhow::Result<Self> {
        let actor = NetworkActor::new(config)?;
        // Establish and validate the netns tree once, here at daemon startup. A
        // confirmed root-0700 tree under sticky /tmp cannot then be tampered by
        // non-root, so the actor's handlers trust it without re-checking; fail
        // closed if it cannot be established.
        netns::ensure_netns_tree_trusted().context("netns tree failed its trust check")?;
        let actor = spawn_with::<NetworkActor>(actor);
        Ok(Self { actor })
    }

    /// Idempotently create the package's named network namespace.
    ///
    /// # Errors
    ///
    /// Returns an error if the actor has stopped or namespace creation fails.
    pub async fn create_netns(&self, pkg: &str) -> anyhow::Result<()> {
        self.actor
            .ask(messages::CreateNetns {
                pkg: pkg.to_owned(),
            })
            .await
            .context("NetworkActor has died?")?
    }

    /// Idempotently attach veth + IP + NAT for the package onto the bridge
    /// resolved from `bridge_id` (`network_name` or the package name).
    ///
    /// # Errors
    ///
    /// Returns an error if the actor has stopped, the pool/subnet is exhausted, or
    /// netavark setup fails.
    pub async fn attach(
        &self,
        pkg: &str,
        bridge_id: &str,
        container_interface: &str,
    ) -> anyhow::Result<()> {
        self.actor
            .ask(messages::Attach {
                pkg: pkg.to_owned(),
                bridge_id: bridge_id.to_owned(),
                container_interface: container_interface.to_owned(),
            })
            .await
            .context("NetworkActor has died?")?
    }

    /// Idempotently detach the package from the bridge resolved from `bridge_id`.
    ///
    /// # Errors
    ///
    /// Returns an error only if the actor has stopped; teardown itself is
    /// best-effort and always reports success.
    pub async fn detach(&self, pkg: &str, bridge_id: &str) -> anyhow::Result<()> {
        self.actor
            .ask(messages::Detach {
                pkg: pkg.to_owned(),
                bridge_id: bridge_id.to_owned(),
            })
            .await
            .context("NetworkActor has died?")?
    }

    /// Idempotently destroy the package's named namespace. Absent = success.
    ///
    /// # Errors
    ///
    /// Returns an error if the actor has stopped or namespace deletion fails.
    pub async fn destroy_netns(&self, pkg: &str) -> anyhow::Result<()> {
        self.actor
            .ask(messages::DestroyNetns {
                pkg: pkg.to_owned(),
            })
            .await
            .context("NetworkActor has died?")?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(base: &str) -> crate::configuration::BridgeConfig {
        crate::configuration::BridgeConfig {
            enabled: true,
            addr_pool: Some(crate::configuration::PoolConfig {
                base: Some(base.to_owned()),
                size: Some(29),
            }),
        }
    }

    #[test]
    fn new_parses_valid_pool() {
        let actor = NetworkActor::new(&config("172.20.0.0/16")).unwrap();
        assert!(actor.bridges.is_empty());
    }

    #[test]
    fn new_defaults_size_when_omitted() {
        let mut cfg = config("172.20.0.0/16");
        cfg.addr_pool.as_mut().unwrap().size = None;
        // Omitted size defaults to /29 (a valid slot in /16), so construction
        // succeeds.
        NetworkActor::new(&cfg).unwrap();
    }

    #[test]
    fn new_errors_on_bad_base() {
        let err = NetworkActor::new(&config("not-a-pool")).unwrap_err();
        assert!(err.to_string().contains("base"));
    }

    #[test]
    fn apply_restore_plan_attach_registers_bridge_and_ip() {
        let mut actor = NetworkActor::new(&config("172.20.0.0/16")).unwrap();
        let br = crate::network::bridge_name("x");
        let veth = allocator::veth_host_name("x");
        let plan = vec![restore::RestoreEntry::Attach(restore::RestoredAttach {
            br: br.clone(),
            subnet: "172.20.0.0/29".parse().unwrap(),
            veth: veth.clone(),
            ip: "172.20.0.1".parse().unwrap(),
        })];
        actor.apply_restore_plan(plan);
        assert!(actor.bridges.contains_key(&br));
        assert_eq!(
            actor.bridges[&br].ip_for(&veth),
            Some("172.20.0.1".parse().unwrap())
        );
    }

    #[test]
    fn apply_restore_plan_reserve_blocks_reissue() {
        let mut actor = NetworkActor::new(&config("172.20.0.0/16")).unwrap();
        let plan = vec![restore::RestoreEntry::ReserveIp {
            ip: "172.20.0.3".parse().unwrap(),
            veth: allocator::veth_host_name("unknown"),
            master: None,
        }];
        actor.apply_restore_plan(plan);
        // .3 sits in slot 172.20.0.0/29; a fresh subnet alloc must skip it.
        assert_eq!(
            actor.subnets.allocate("fresh").unwrap(),
            "172.20.0.8/29".parse().unwrap()
        );
    }

    #[test]
    fn apply_restore_plan_attach_wins_over_reserve_in_same_slot() {
        let mut actor = NetworkActor::new(&config("172.20.0.0/16")).unwrap();
        let br = crate::network::bridge_name("x");
        let veth = allocator::veth_host_name("x");
        let demoted_veth = allocator::veth_host_name("y");
        let plan = vec![
            restore::RestoreEntry::ReserveIp {
                ip: "172.20.0.3".parse().unwrap(),
                veth: demoted_veth.clone(),
                master: Some(br.clone()),
            },
            restore::RestoreEntry::Attach(restore::RestoredAttach {
                br: br.clone(),
                subnet: "172.20.0.0/29".parse().unwrap(),
                veth: veth.clone(),
                ip: "172.20.0.1".parse().unwrap(),
            }),
        ];
        actor.apply_restore_plan(plan);
        // Sibling's own IP is untouched.
        assert_eq!(
            actor.bridges[&br].ip_for(&veth),
            Some("172.20.0.1".parse().unwrap())
        );
        // Demoted sibling's IP claimed under its real veth id — protected too.
        assert_eq!(
            actor.bridges[&br].ip_for(&demoted_veth),
            Some("172.20.0.3".parse().unwrap())
        );
        assert_eq!(
            actor
                .bridges
                .get_mut(&br)
                .unwrap()
                .allocate("new-sibling-1")
                .unwrap(),
            "172.20.0.2".parse::<Ipv4Addr>().unwrap()
        );
        assert_eq!(
            actor
                .bridges
                .get_mut(&br)
                .unwrap()
                .allocate("new-sibling-2")
                .unwrap(),
            "172.20.0.4".parse::<Ipv4Addr>().unwrap()
        );
        // Real key: detach frees it — no permanent leak.
        actor.bridges.get_mut(&br).unwrap().release(&demoted_veth);
        assert_eq!(
            actor
                .bridges
                .get_mut(&br)
                .unwrap()
                .allocate("new-sibling-3")
                .unwrap(),
            "172.20.0.3".parse::<Ipv4Addr>().unwrap()
        );
    }

    #[test]
    fn apply_restore_plan_reserve_falls_back_when_bridge_unseen() {
        // Known master, but its bridge is unseen this boot — falls back.
        let mut actor = NetworkActor::new(&config("172.20.0.0/16")).unwrap();
        let br = crate::network::bridge_name("x");
        let plan = vec![restore::RestoreEntry::ReserveIp {
            ip: "172.20.0.3".parse().unwrap(),
            veth: allocator::veth_host_name("y"),
            master: Some(br.clone()),
        }];
        actor.apply_restore_plan(plan);
        assert!(!actor.bridges.contains_key(&br));
        assert_eq!(
            actor.subnets.allocate("fresh").unwrap(),
            "172.20.0.8/29".parse().unwrap()
        );
    }

    #[test]
    fn rollback_attach_destroys_now_empty_bridge() {
        let mut actor = NetworkActor::new(&config("172.20.0.0/16")).unwrap();
        let br = crate::network::bridge_name("x");
        let veth = allocator::veth_host_name("x");
        actor.allocate_container_ip(&br, &veth).unwrap();
        assert!(actor.bridges.contains_key(&br));
        actor.rollback_attach(&br, &veth);
        assert!(!actor.bridges.contains_key(&br));
    }

    #[tokio::test]
    #[ignore = "requires root + CLONE_NEWNET; run under the Docker test harness"]
    async fn create_netns_idempotent() {
        let mgr = NetworkManager::new(&config("172.20.0.0/16")).unwrap();
        let pkg = "ssam-test-create";
        let name = netns::netns_name(pkg);
        let path = netns::netns_path(&name);

        mgr.create_netns(pkg).await.unwrap();
        assert!(path.exists());
        // Second create must not destroy the existing namespace.
        mgr.create_netns(pkg).await.unwrap();
        assert!(path.exists());

        mgr.destroy_netns(pkg).await.unwrap();
    }
}
