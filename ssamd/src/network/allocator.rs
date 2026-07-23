// Copyright 2026 Hyundai Mobis Co., Ltd.
// SPDX-License-Identifier: Apache-2.0

//! Per-package naming and in-memory IP allocation for the bridge network.
//!
//! Naming (`veth_host_name`, netns name) is deterministic from the package id
//! so host interfaces and namespaces stay in lockstep. IP **allocation** hands
//! out the lowest free host address by a linear scan for the lowest unheld key,
//! which guarantees no two running packages share an address — important because
//! packages are installed/updated in the field with uncontrolled names, where a
//! name-hashed IP would collide.
//!
//! The allocation state lives only in memory with no persistent lease, so it
//! starts empty on every daemon start. Containers can outlive the daemon, so a
//! restart repopulates it by scanning in-use container namespaces and
//! re-claiming their live addresses (see [`BridgeIpAllocator::claim`]) before any new
//! allocation, which keeps a survivor's address from being reissued.

use std::collections::HashMap;
use std::net::Ipv4Addr;

use anyhow::Context as _;
use ipnet::Ipv4Net;

/// Host-side veth interface name for a package (15 chars, `IFNAMSIZ - 1`).
#[must_use]
pub fn veth_host_name(pkg: &str) -> String {
    format!("ssam-{}", crate::network::pkg_hash10(pkg))
}

/// Lowest-free allocator over `u32` slot keys. Backs both [`BridgeIpAllocator`]
/// (host addresses) and [`SubnetAllocator`] (slot network addresses).
#[derive(Debug, Default)]
struct SlotAllocator {
    /// Key held by each id; a key absent from the map is free.
    attached: HashMap<String, u32>,
}

impl SlotAllocator {
    /// Lowest free key for `id`, or its existing one. `None` when no in-range
    /// key is free.
    fn allocate(
        &mut self,
        id: &str,
        start: u32,
        step: u32,
        in_range: impl Fn(u32) -> bool,
    ) -> Option<u32> {
        if let Some(&existing) = self.attached.get(id) {
            return Some(existing);
        }
        let candidate = std::iter::successors(Some(start), |&slot| slot.checked_add(step))
            .take_while(|&slot| in_range(slot))
            .find(|&slot| !self.attached.values().any(|&held| held == slot))?;
        self.attached.insert(id.to_owned(), candidate);
        Some(candidate)
    }

    /// Claim `key` for `id`. Re-claiming `id` to a new key frees the old.
    /// `Err(owner)`, without changing state, if a different id already holds `key`.
    fn claim(&mut self, id: &str, key: u32) -> Result<(), String> {
        if let Some((owner, _)) = self
            .attached
            .iter()
            .find(|&(owner, &owned)| owned == key && owner != id)
        {
            return Err(owner.clone());
        }
        self.attached.insert(id.to_owned(), key);
        Ok(())
    }

    /// Drop `id`'s attachment so its key is free again. Absent id is a no-op.
    fn release(&mut self, id: &str) {
        self.attached.remove(id);
    }

    fn get(&self, id: &str) -> Option<u32> {
        self.attached.get(id).copied()
    }

    fn is_empty(&self) -> bool {
        self.attached.is_empty()
    }
}

/// In-memory tracker of which network id (`ssam-<hash>`, the veth/netns name)
/// holds which address.
///
/// Keyed by the deterministic id (not the package name) so the in-use scan,
/// which only sees `ssam-<hash>` netns entries, can register an address
/// directly.
#[derive(Debug)]
pub struct BridgeIpAllocator {
    subnet: Ipv4Net,
    slots: SlotAllocator,
}

impl BridgeIpAllocator {
    #[must_use]
    pub fn new(subnet: Ipv4Net) -> Self {
        Self {
            subnet,
            slots: SlotAllocator::default(),
        }
    }

    /// The subnet this allocator hands host addresses out of.
    #[must_use]
    pub fn subnet(&self) -> Ipv4Net {
        self.subnet
    }

    /// Allocate (or return the existing) IP for `netid` as the lowest free host
    /// address in the subnet — its `.1` on up. The network and broadcast addresses
    /// are reserved; an internal bridge has no gateway, so `.1` is a usable host.
    ///
    /// Idempotent for a repeated `netid`. Guarantees no two tracked ids share an
    /// address.
    ///
    /// # Errors
    ///
    /// Returns an error if the subnet has no free host address left.
    pub fn allocate(&mut self, netid: &str) -> anyhow::Result<Ipv4Addr> {
        let broadcast = u32::from(self.subnet.broadcast());
        let first_host = u32::from(self.subnet.network()) + 1;
        let key = self
            .slots
            .allocate(netid, first_host, 1, |c| c < broadcast)
            .with_context(|| format!("Subnet {} has no free host address", self.subnet))?;
        Ok(Ipv4Addr::from(key))
    }

    /// Claim `ip` for `netid` — record an in-use address so a fresh allocation
    /// never reissues it. Re-claiming a new address for the same id frees the old.
    ///
    /// # Errors
    ///
    /// Errors, without changing state, if `ip` is not a usable host of the subnet
    /// (network and broadcast are excluded) or a different `netid` already owns `ip`.
    pub fn claim(&mut self, netid: &str, ip: Ipv4Addr) -> anyhow::Result<()> {
        let ip_key = u32::from(ip);
        let first_host = u32::from(self.subnet.network()) + 1;
        let broadcast = u32::from(self.subnet.broadcast());
        anyhow::ensure!(
            (first_host..broadcast).contains(&ip_key),
            "address {ip} is not a usable host of bridge subnet {}",
            self.subnet
        );
        self.slots.claim(netid, ip_key).map_err(|owner| {
            anyhow::anyhow!(
                "address {ip} already claimed by {owner}; refusing to reassign to {netid}"
            )
        })
    }

    /// Remove the attachment for `netid`, returning its address to the free-list.
    /// Absent entries are a no-op.
    pub fn release(&mut self, netid: &str) {
        self.slots.release(netid);
    }

    /// The address currently attached to `netid`, if any. Read-only — never
    /// mints a fresh address.
    #[must_use]
    pub fn ip_for(&self, netid: &str) -> Option<Ipv4Addr> {
        self.slots.get(netid).map(Ipv4Addr::from)
    }

    /// Whether no address is currently attached (the bridge is unused).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }
}

/// Carves a `pool` supernet into fixed-size `/subnet_prefix` slots, one per
/// bridge — the same [`SlotAllocator`] one level up (pool → subnet).
#[derive(Debug)]
pub struct SubnetAllocator {
    pool: Ipv4Net,
    subnet_prefix: u8,
    /// Slot size in addresses (`1 << (32 - subnet_prefix)`).
    step: u32,
    /// Slot network addresses keyed by bridge interface name.
    slots: SlotAllocator,
}

impl SubnetAllocator {
    /// # Errors
    ///
    /// Errors when `subnet_prefix` is not in `[pool.prefix_len().max(1), 32]` —
    /// i.e. the pool is too small to hold one slot, or the prefix would leave no
    /// slot step.
    pub fn new(pool: Ipv4Net, subnet_prefix: u8) -> anyhow::Result<Self> {
        let min = pool.prefix_len().max(1);
        anyhow::ensure!(
            (min..=32).contains(&subnet_prefix),
            "subnet_prefix /{subnet_prefix} out of range [/{min}, /32] for pool {pool}"
        );
        let step = 1u32 << (32 - subnet_prefix); // subnet_prefix in [1, 32] => shift in [0, 31]
        Ok(Self {
            pool,
            subnet_prefix,
            step,
            slots: SlotAllocator::default(),
        })
    }

    /// Lowest free `/subnet_prefix` subnet for `bridge_name`. Idempotent per key.
    ///
    /// # Errors
    ///
    /// Errors when the pool has no free slot left.
    pub fn allocate(&mut self, bridge_name: &str) -> anyhow::Result<Ipv4Net> {
        let pool_broadcast = u32::from(self.pool.broadcast());
        let start = u32::from(self.pool.network());
        let step = self.step;
        let addr = self
            .slots
            .allocate(bridge_name, start, step, |c| {
                c.saturating_add(step - 1) <= pool_broadcast
            })
            .with_context(|| {
                format!(
                    "pool {} exhausted for /{} bridge subnets",
                    self.pool, self.subnet_prefix
                )
            })?;
        self.slot(addr)
    }

    /// The `/subnet_prefix` net whose network address is `addr`. Only fails on a
    /// corrupt prefix — `addr` is always a valid in-pool slot base.
    fn slot(&self, addr: u32) -> anyhow::Result<Ipv4Net> {
        Ipv4Net::new(Ipv4Addr::from(addr), self.subnet_prefix).context("invalid bridge subnet")
    }

    /// Claim `subnet` for `bridge_name` on boot restore. Re-claiming the same
    /// bridge to a new subnet frees the old.
    ///
    /// # Errors
    ///
    /// Errors, without changing state, if `subnet` is not a `/subnet_prefix` slot
    /// aligned inside the pool, or if a different bridge already owns it.
    pub fn claim(&mut self, bridge_name: &str, subnet: Ipv4Net) -> anyhow::Result<()> {
        anyhow::ensure!(
            subnet.prefix_len() == self.subnet_prefix,
            "subnet {subnet} prefix != pool slot /{}",
            self.subnet_prefix
        );
        let addr = u32::from(subnet.network());
        let pool_network = u32::from(self.pool.network());
        let pool_broadcast = u32::from(self.pool.broadcast());
        anyhow::ensure!(
            addr >= pool_network && addr.saturating_add(self.step - 1) <= pool_broadcast,
            "subnet {subnet} is outside pool {}",
            self.pool
        );
        self.slots.claim(bridge_name, addr).map_err(|owner| {
            anyhow::anyhow!(
                "subnet {subnet} already claimed by {owner}; refusing to reassign to {bridge_name}"
            )
        })
    }

    /// Release a bridge's subnet to the free-list. Absent key is a no-op.
    pub fn release(&mut self, bridge_name: &str) {
        self.slots.release(bridge_name);
    }

    /// Hold the `/subnet_prefix` slot containing `ip` under a synthetic key.
    /// Best-effort: a foreign-owned or out-of-pool slot is left untouched.
    ///
    /// ponytail: reserved slots are only cleared by the next restart's rescan.
    pub fn reserve_containing(&mut self, ip: Ipv4Addr) {
        let Ok(slot) = Ipv4Net::new(ip, self.subnet_prefix).map(|net| net.trunc()) else {
            return;
        };
        // A foreign-owned slot is already protected: ignore a claim conflict.
        let _ = self.claim(&format!("reserved:{}", slot.network()), slot);
    }

    /// The supernet this allocator carves bridge subnets from.
    #[must_use]
    pub fn pool(&self) -> Ipv4Net {
        self.pool
    }

    /// The per-bridge subnet prefix length carved from the pool.
    #[must_use]
    pub fn subnet_prefix(&self) -> u8 {
        self.subnet_prefix
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn net29() -> Ipv4Net {
        // 10.0.0.0/29: network .0, broadcast .7 reserved -> usable .1..=.6.
        "10.0.0.0/29".parse().unwrap()
    }

    #[test]
    fn veth_host_name_is_stable_and_sized() {
        let name = veth_host_name("foo");
        assert_eq!(name, veth_host_name("foo"));
        assert_eq!(name.len(), 15);
        assert!(name.starts_with("ssam-"));
    }

    #[test]
    fn allocate_skips_network_broadcast() {
        let mut alloc = BridgeIpAllocator::new(net29());
        // .0 network is skipped; the lowest usable host is .1 (no gateway).
        assert_eq!(alloc.allocate("a").unwrap(), Ipv4Addr::new(10, 0, 0, 1));
    }

    #[test]
    fn allocate_errors_on_exhaustion() {
        let mut alloc = BridgeIpAllocator::new(net29());
        // /29 has 6 usable hosts (.1..=.6); the 7th allocation fails.
        for i in 0..6 {
            alloc.allocate(&format!("p{i}")).unwrap();
        }
        assert!(alloc.allocate("overflow").is_err());
    }

    #[test]
    fn allocate_is_idempotent() {
        let mut alloc = BridgeIpAllocator::new(net29());
        let first = alloc.allocate("foo").unwrap();
        let again = alloc.allocate("foo").unwrap();
        assert_eq!(first, again);
    }

    #[test]
    fn allocate_gives_distinct_sequential_ips() {
        let mut alloc = BridgeIpAllocator::new(net29());
        assert_eq!(alloc.allocate("a").unwrap(), Ipv4Addr::new(10, 0, 0, 1));
        assert_eq!(alloc.allocate("b").unwrap(), Ipv4Addr::new(10, 0, 0, 2));
        assert_eq!(alloc.allocate("c").unwrap(), Ipv4Addr::new(10, 0, 0, 3));
    }

    #[test]
    fn release_frees_the_lowest_slot_for_reuse() {
        let mut alloc = BridgeIpAllocator::new(net29());
        let a = alloc.allocate("a").unwrap(); // .1
        let _b = alloc.allocate("b").unwrap(); // .2
        alloc.release("a");
        // .1 is free again and is the lowest, so a new package reuses it.
        assert_eq!(alloc.allocate("c").unwrap(), a);
        alloc.release("ghost"); // absent key is a no-op
    }

    #[test]
    fn reuse_prefers_lowest_freed_then_high_water() {
        let mut alloc = BridgeIpAllocator::new(net29());
        let _a = alloc.allocate("a").unwrap(); // .1
        let _b = alloc.allocate("b").unwrap(); // .2
        let _c = alloc.allocate("c").unwrap(); // .3
        alloc.release("b"); // frees .2
        // Lowest free is the released .2, not the high-water .4.
        assert_eq!(alloc.allocate("d").unwrap(), Ipv4Addr::new(10, 0, 0, 2));
        // With the free-list drained, allocation resumes at the high-water .4.
        assert_eq!(alloc.allocate("e").unwrap(), Ipv4Addr::new(10, 0, 0, 4));
    }

    #[test]
    fn claim_holds_address_and_blocks_reissue() {
        let mut alloc = BridgeIpAllocator::new(net29());
        let claimed = Ipv4Addr::new(10, 0, 0, 2);
        alloc.claim("inuse", claimed).unwrap();
        assert_eq!(alloc.allocate("inuse").unwrap(), claimed);
        assert_eq!(alloc.allocate("new").unwrap(), Ipv4Addr::new(10, 0, 0, 1));
    }

    #[test]
    fn claim_then_release_frees_the_address() {
        let mut alloc = BridgeIpAllocator::new(net29());
        let ip = Ipv4Addr::new(10, 0, 0, 1);
        alloc.claim("inuse", ip).unwrap();
        assert_eq!(alloc.allocate("inuse").unwrap(), ip);
        // Releasing a claimed id returns the address to reuse.
        alloc.release("inuse");
        assert_eq!(alloc.allocate("new").unwrap(), ip);
    }

    #[test]
    fn claim_rejects_address_owned_by_another_id() {
        let mut alloc = BridgeIpAllocator::new(net29());
        let ip = Ipv4Addr::new(10, 0, 0, 2);
        alloc.claim("first", ip).unwrap();
        // A duplicate claim by a different id must not steal the address:
        // two ids mapping to one IP would let both allocate it.
        assert!(alloc.claim("second", ip).is_err());
        assert_eq!(alloc.allocate("first").unwrap(), ip);
        assert_eq!(
            alloc.allocate("second").unwrap(),
            Ipv4Addr::new(10, 0, 0, 1)
        );
    }

    #[test]
    fn claim_same_id_same_address_is_idempotent() {
        let mut alloc = BridgeIpAllocator::new(net29());
        let ip = Ipv4Addr::new(10, 0, 0, 2);
        alloc.claim("inuse", ip).unwrap();
        alloc.claim("inuse", ip).unwrap();
        assert_eq!(alloc.allocate("inuse").unwrap(), ip);
    }

    #[test]
    fn claim_rejects_network_and_broadcast() {
        let mut alloc = BridgeIpAllocator::new(net29());
        // .0 network and .7 broadcast are not usable hosts; a claim of either
        // must fail so a corrupt restore can never mark them in-use.
        assert!(alloc.claim("net", Ipv4Addr::new(10, 0, 0, 0)).is_err());
        assert!(alloc.claim("bcast", Ipv4Addr::new(10, 0, 0, 7)).is_err());
        // Neighboring usable hosts still claim fine.
        alloc.claim("host", Ipv4Addr::new(10, 0, 0, 6)).unwrap();
    }

    #[test]
    fn claim_high_address_does_not_strand_low_gap() {
        let mut alloc = BridgeIpAllocator::new(net29());
        // Claim the highest host; the cursor must NOT jump past it, so the lower
        // hosts stay allocatable (the old cursor-bump would exhaust the subnet).
        alloc.claim("inuse", Ipv4Addr::new(10, 0, 0, 6)).unwrap();
        assert_eq!(alloc.allocate("a").unwrap(), Ipv4Addr::new(10, 0, 0, 1));
        assert_eq!(alloc.allocate("b").unwrap(), Ipv4Addr::new(10, 0, 0, 2));
        // The claimed address is still held: the owner keeps it.
        assert_eq!(alloc.allocate("inuse").unwrap(), Ipv4Addr::new(10, 0, 0, 6));
    }

    #[test]
    fn reclaim_same_id_new_address_frees_old() {
        let mut alloc = BridgeIpAllocator::new(net29());
        alloc.claim("inuse", Ipv4Addr::new(10, 0, 0, 4)).unwrap();
        // Re-claiming the same id to a new address must free the old one, not
        // orphan it: .4 stays allocatable, handed out once lower slots fill.
        alloc.claim("inuse", Ipv4Addr::new(10, 0, 0, 5)).unwrap();
        assert_eq!(alloc.allocate("inuse").unwrap(), Ipv4Addr::new(10, 0, 0, 5));
        let _a = alloc.allocate("a").unwrap(); // .1
        let _b = alloc.allocate("b").unwrap(); // .2
        let _c = alloc.allocate("c").unwrap(); // .3
        // The freed .4 is reused, proving the reclaim returned it to the pool.
        assert_eq!(alloc.allocate("d").unwrap(), Ipv4Addr::new(10, 0, 0, 4));
    }

    fn pool16() -> Ipv4Net {
        "10.0.0.0/16".parse().unwrap()
    }

    #[test]
    fn subnet_new_rejects_prefix_out_of_range() {
        let pool: Ipv4Net = "10.0.0.0/30".parse().unwrap();
        assert!(SubnetAllocator::new(pool, 29).is_err()); // pool coarser than slot
        assert!(SubnetAllocator::new(pool16(), 40).is_err()); // > /32, no u8 underflow
    }

    #[test]
    fn subnet_allocate_gives_distinct_sequential_slots() {
        let mut alloc = SubnetAllocator::new(pool16(), 29).unwrap();
        assert_eq!(alloc.allocate("a").unwrap(), "10.0.0.0/29".parse().unwrap());
        assert_eq!(alloc.allocate("b").unwrap(), "10.0.0.8/29".parse().unwrap());
        assert_eq!(
            alloc.allocate("c").unwrap(),
            "10.0.0.16/29".parse().unwrap()
        );
    }

    #[test]
    fn subnet_allocate_is_idempotent() {
        let mut alloc = SubnetAllocator::new(pool16(), 29).unwrap();
        let first = alloc.allocate("foo").unwrap();
        assert_eq!(first, alloc.allocate("foo").unwrap());
    }

    #[test]
    fn subnet_allocate_errors_on_exhaustion() {
        // /30 pool with /30 slots yields exactly one slot; the second fails.
        let pool: Ipv4Net = "10.0.0.0/30".parse().unwrap();
        let mut alloc = SubnetAllocator::new(pool, 30).unwrap();
        assert_eq!(alloc.allocate("a").unwrap(), pool);
        assert!(
            alloc
                .allocate("b")
                .unwrap_err()
                .to_string()
                .contains("exhausted")
        );
    }

    #[test]
    fn subnet_release_reuses_lowest_freed_before_high_water() {
        let mut alloc = SubnetAllocator::new(pool16(), 29).unwrap();
        let _a = alloc.allocate("a").unwrap(); // .0/29
        let b = alloc.allocate("b").unwrap(); // .8/29
        let _c = alloc.allocate("c").unwrap(); // .16/29
        alloc.release("b");
        // Lowest free is the released .8/29, not the high-water .24/29.
        assert_eq!(alloc.allocate("d").unwrap(), b);
        assert_eq!(
            alloc.allocate("e").unwrap(),
            "10.0.0.24/29".parse().unwrap()
        );
    }

    #[test]
    fn subnet_claim_holds_slot_and_blocks_reissue() {
        let mut alloc = SubnetAllocator::new(pool16(), 29).unwrap();
        let held: Ipv4Net = "10.0.0.0/29".parse().unwrap();
        alloc.claim("inuse", held).unwrap();
        // A fresh bridge skips the claimed slot.
        assert_eq!(
            alloc.allocate("new").unwrap(),
            "10.0.0.8/29".parse().unwrap()
        );
    }

    #[test]
    fn subnet_claim_rejects_foreign_owned_slot() {
        let mut alloc = SubnetAllocator::new(pool16(), 29).unwrap();
        let s: Ipv4Net = "10.0.0.0/29".parse().unwrap();
        alloc.claim("first", s).unwrap();
        assert!(alloc.claim("second", s).is_err());
    }

    #[test]
    fn subnet_claim_rejects_wrong_prefix_or_out_of_pool() {
        let mut alloc = SubnetAllocator::new(pool16(), 29).unwrap();
        // Out of pool.
        assert!(alloc.claim("y", "10.1.0.0/29".parse().unwrap()).is_err());
        // Wrong prefix (not a /29 slot).
        assert!(alloc.claim("z", "10.0.0.0/28".parse().unwrap()).is_err());
    }

    #[test]
    fn reserve_containing_holds_slot_for_fresh_allocate() {
        let mut alloc = SubnetAllocator::new(pool16(), 29).unwrap();
        alloc.reserve_containing(Ipv4Addr::new(10, 0, 0, 3)); // slot 10.0.0.0/29
        assert_eq!(
            alloc.allocate("new").unwrap(),
            "10.0.0.8/29".parse().unwrap()
        );
    }

    #[test]
    fn reserve_containing_is_idempotent_per_slot() {
        let mut alloc = SubnetAllocator::new(pool16(), 29).unwrap();
        alloc.reserve_containing(Ipv4Addr::new(10, 0, 0, 3));
        alloc.reserve_containing(Ipv4Addr::new(10, 0, 0, 5)); // same /29 slot
        assert_eq!(
            alloc.allocate("new").unwrap(),
            "10.0.0.8/29".parse().unwrap()
        );
    }

    #[test]
    fn reserve_containing_is_noop_when_slot_owned_by_a_bridge() {
        let mut alloc = SubnetAllocator::new(pool16(), 29).unwrap();
        let held = alloc.allocate("a").unwrap(); // 10.0.0.0/29
        alloc.reserve_containing(Ipv4Addr::new(10, 0, 0, 2)); // inside the bridge's own slot
        // The real bridge keeps its slot; reserve neither stole nor duplicated it.
        assert_eq!(alloc.allocate("a").unwrap(), held);
    }

    #[test]
    fn reserve_containing_is_noop_out_of_pool() {
        let mut alloc = SubnetAllocator::new(pool16(), 29).unwrap();
        alloc.reserve_containing(Ipv4Addr::new(10, 1, 0, 3)); // outside 10.0.0.0/16
        assert_eq!(alloc.allocate("a").unwrap(), "10.0.0.0/29".parse().unwrap());
    }
}
