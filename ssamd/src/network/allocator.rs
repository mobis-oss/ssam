// Copyright 2026 Hyundai Mobis Co., Ltd.
// SPDX-License-Identifier: Apache-2.0

//! Per-package naming and in-memory IP allocation for the bridge network.
//!
//! Naming (`veth_host_name`, netns name) is deterministic from the package id
//! so host interfaces and namespaces stay in lockstep. IP **allocation** hands
//! out the lowest free host address via a high-water cursor plus a free-list of
//! released addresses (O(log n) per allocate/release), which guarantees no two
//! running packages share an address — important because packages are
//! installed/updated in the field with uncontrolled names, where a name-hashed
//! IP would collide.
//!
//! The allocation state lives only in memory with no persistent lease, so it
//! starts empty on every daemon start. Containers can outlive the daemon, so a
//! restart repopulates it by scanning in-use container namespaces and
//! re-claiming their live addresses (see [`IpAllocator::claim`]) before any new
//! allocation, which keeps a survivor's address from being reissued.

use std::collections::{BTreeSet, HashMap};
use std::net::Ipv4Addr;

use ipnet::Ipv4Net;

/// Host-side veth interface name for a package (15 chars, `IFNAMSIZ - 1`).
#[must_use]
pub fn veth_host_name(pkg: &str) -> String {
    format!("ssam-{}", crate::network::pkg_hash10(pkg))
}

/// In-memory tracker of which network id (`ssam-<hash>`, the veth/netns name)
/// holds which address.
///
/// Keyed by the deterministic id (not the package name) so the in-use scan,
/// which only sees `ssam-<hash>` netns entries, can register an address
/// directly. Allocation returns the lowest free host address: the `freed` set
/// is drained lowest-first, then the `next` high-water cursor extends into
/// never-assigned space. `next_free` skips the gateway and any `attached`
/// address, so a claimed address below the cursor is never reissued.
#[derive(Debug, Default)]
pub struct IpAllocator {
    /// Address held by each network id (the deterministic `ssam-<hash>` name).
    attached: HashMap<String, Ipv4Addr>,
    /// Released host addresses (as `u32`), sorted so the lowest is reused first.
    freed: BTreeSet<u32>,
    /// High-water mark: the next never-yet-assigned host address. `None` until
    /// the first allocation derives it from the subnet.
    next: Option<u32>,
}

impl IpAllocator {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Allocate (or return the existing) IP for `netid` as the lowest free host
    /// address in `subnet`.
    ///
    /// Idempotent for a repeated `netid`. Guarantees no two tracked ids share an
    /// address.
    ///
    /// # Errors
    ///
    /// Returns an error if the subnet has no free host address left.
    pub fn allocate(
        &mut self,
        netid: &str,
        subnet: &Ipv4Net,
        gateway: Ipv4Addr,
    ) -> anyhow::Result<Ipv4Addr> {
        if let Some(existing) = self.attached.get(netid) {
            return Ok(*existing);
        }

        let ip = self.next_free(*subnet, gateway)?;
        self.attached.insert(netid.to_owned(), ip);
        Ok(ip)
    }

    /// Lowest free host address: reuse the smallest released slot before
    /// extending the high-water cursor past the gateway toward the broadcast.
    fn next_free(&mut self, subnet: Ipv4Net, gateway: Ipv4Addr) -> anyhow::Result<Ipv4Addr> {
        if let Some(&low) = self.freed.iter().next() {
            self.freed.remove(&low);
            return Ok(Ipv4Addr::from(low));
        }

        let broadcast = u32::from(subnet.broadcast());
        let gateway_u32 = u32::from(gateway);
        let first_host = u32::from(subnet.network()) + 1;

        // Advance past the gateway and any `attached` address (e.g. a claimed
        // in-use container at or below the cursor). Skipping owned addresses
        // here, rather than bumping the cursor on claim, keeps the low range
        // dense. checked_add guards the overflow-at-top case (saturates to
        // broadcast, falling through to exhaustion).
        let mut candidate = self.next.unwrap_or(first_host);
        while candidate < broadcast
            && (candidate == gateway_u32
                || self
                    .attached
                    .values()
                    .any(|owned| u32::from(*owned) == candidate))
        {
            candidate = candidate.checked_add(1).unwrap_or(broadcast);
        }
        anyhow::ensure!(
            candidate < broadcast,
            "Subnet {subnet} has no free host address (gateway {gateway})"
        );
        self.next = Some(candidate + 1);
        Ok(Ipv4Addr::from(candidate))
    }

    /// Claim `ip` for `netid` — record an in-use address so a fresh allocation
    /// never reissues it. Held in `attached` (which `next_free` skips) without
    /// advancing the cursor. Re-claiming a new address for the same id frees the
    /// old one.
    ///
    /// # Errors
    ///
    /// Errors without changing state if a different `netid` already owns `ip`.
    pub fn claim(&mut self, netid: &str, ip: Ipv4Addr) -> anyhow::Result<()> {
        if let Some((owner, _)) = self
            .attached
            .iter()
            .find(|&(owner, &owned)| owned == ip && owner != netid)
        {
            anyhow::bail!(
                "address {ip} already claimed by {owner}; refusing to reassign to {netid}"
            );
        }
        let host = u32::from(ip);
        // Re-claiming the same id to a new address must release the old one to
        // the free-list, not orphan it.
        if let Some(&old) = self.attached.get(netid)
            && old != ip
        {
            self.freed.insert(u32::from(old));
        }
        self.attached.insert(netid.to_owned(), ip);
        self.freed.remove(&host);
        Ok(())
    }

    /// Remove the attachment for `netid`, returning its address to the free-list.
    /// Absent entries are a no-op.
    pub fn release(&mut self, netid: &str) {
        if let Some(ip) = self.attached.remove(netid) {
            self.freed.insert(u32::from(ip));
        }
    }

    /// The address currently attached to `netid`, if any. Read-only — never
    /// mints a fresh address, so callers (e.g. netavark teardown) act on the
    /// real address, never a synthesized one.
    #[must_use]
    pub fn ip_for(&self, netid: &str) -> Option<Ipv4Addr> {
        self.attached.get(netid).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn net29() -> Ipv4Net {
        // 10.0.0.0/29: network .0, broadcast .7, gateway .1 -> usable .2..=.6.
        "10.0.0.0/29".parse().unwrap()
    }

    fn gw() -> Ipv4Addr {
        Ipv4Addr::new(10, 0, 0, 1)
    }

    #[test]
    fn veth_host_name_is_stable_and_sized() {
        let name = veth_host_name("foo");
        assert_eq!(name, veth_host_name("foo"));
        assert_eq!(name.len(), 15);
        assert!(name.starts_with("ssam-"));
    }

    #[test]
    fn allocate_skips_network_gateway_broadcast() {
        let net = net29();
        let mut alloc = IpAllocator::new();
        // .0 network and .1 gateway are skipped; the lowest usable host is .2.
        assert_eq!(
            alloc.allocate("a", &net, gw()).unwrap(),
            Ipv4Addr::new(10, 0, 0, 2)
        );
    }

    #[test]
    fn allocate_errors_on_exhaustion() {
        let net = net29();
        let mut alloc = IpAllocator::new();
        // /29 has 5 usable hosts (.2..=.6); the 6th allocation fails.
        for i in 0..5 {
            alloc.allocate(&format!("p{i}"), &net, gw()).unwrap();
        }
        assert!(alloc.allocate("overflow", &net, gw()).is_err());
    }

    #[test]
    fn allocate_is_idempotent() {
        let net = net29();
        let mut alloc = IpAllocator::new();
        let first = alloc.allocate("foo", &net, gw()).unwrap();
        let again = alloc.allocate("foo", &net, gw()).unwrap();
        assert_eq!(first, again);
    }

    #[test]
    fn allocate_gives_distinct_sequential_ips() {
        let net = net29();
        let mut alloc = IpAllocator::new();
        assert_eq!(
            alloc.allocate("a", &net, gw()).unwrap(),
            Ipv4Addr::new(10, 0, 0, 2)
        );
        assert_eq!(
            alloc.allocate("b", &net, gw()).unwrap(),
            Ipv4Addr::new(10, 0, 0, 3)
        );
        assert_eq!(
            alloc.allocate("c", &net, gw()).unwrap(),
            Ipv4Addr::new(10, 0, 0, 4)
        );
    }

    #[test]
    fn release_frees_the_lowest_slot_for_reuse() {
        let net = net29();
        let mut alloc = IpAllocator::new();
        let a = alloc.allocate("a", &net, gw()).unwrap(); // .2
        let _b = alloc.allocate("b", &net, gw()).unwrap(); // .3
        alloc.release("a");
        // .2 is free again and is the lowest, so a new package reuses it.
        assert_eq!(alloc.allocate("c", &net, gw()).unwrap(), a);
        alloc.release("ghost"); // absent key is a no-op
    }

    #[test]
    fn reuse_prefers_lowest_freed_then_high_water() {
        let net = net29();
        let mut alloc = IpAllocator::new();
        let _a = alloc.allocate("a", &net, gw()).unwrap(); // .2
        let _b = alloc.allocate("b", &net, gw()).unwrap(); // .3
        let _c = alloc.allocate("c", &net, gw()).unwrap(); // .4
        alloc.release("b"); // frees .3
        // Lowest free is the released .3, not the high-water .5.
        assert_eq!(
            alloc.allocate("d", &net, gw()).unwrap(),
            Ipv4Addr::new(10, 0, 0, 3)
        );
        // With the free-list drained, allocation resumes at the high-water .5.
        assert_eq!(
            alloc.allocate("e", &net, gw()).unwrap(),
            Ipv4Addr::new(10, 0, 0, 5)
        );
    }

    #[test]
    fn claim_holds_address_and_blocks_reissue() {
        let net = net29();
        let mut alloc = IpAllocator::new();
        let claimed = Ipv4Addr::new(10, 0, 0, 2);
        alloc.claim("inuse", claimed).unwrap();
        assert_eq!(alloc.allocate("inuse", &net, gw()).unwrap(), claimed);
        assert_eq!(
            alloc.allocate("new", &net, gw()).unwrap(),
            Ipv4Addr::new(10, 0, 0, 3)
        );
    }

    #[test]
    fn claim_then_release_frees_the_address() {
        let net = net29();
        let mut alloc = IpAllocator::new();
        let ip = Ipv4Addr::new(10, 0, 0, 2);
        alloc.claim("inuse", ip).unwrap();
        assert_eq!(alloc.allocate("inuse", &net, gw()).unwrap(), ip);
        // Releasing a claimed id returns the address to reuse.
        alloc.release("inuse");
        assert_eq!(alloc.allocate("new", &net, gw()).unwrap(), ip);
    }

    #[test]
    fn claim_rejects_address_owned_by_another_id() {
        let net = net29();
        let mut alloc = IpAllocator::new();
        let ip = Ipv4Addr::new(10, 0, 0, 2);
        alloc.claim("first", ip).unwrap();
        // A duplicate claim by a different id must not steal the address:
        // two ids mapping to one IP would let both allocate it.
        assert!(alloc.claim("second", ip).is_err());
        assert_eq!(alloc.allocate("first", &net, gw()).unwrap(), ip);
        assert_eq!(
            alloc.allocate("second", &net, gw()).unwrap(),
            Ipv4Addr::new(10, 0, 0, 3)
        );
    }

    #[test]
    fn claim_same_id_same_address_is_idempotent() {
        let net = net29();
        let mut alloc = IpAllocator::new();
        let ip = Ipv4Addr::new(10, 0, 0, 2);
        alloc.claim("inuse", ip).unwrap();
        alloc.claim("inuse", ip).unwrap();
        assert_eq!(alloc.allocate("inuse", &net, gw()).unwrap(), ip);
    }

    #[test]
    fn claim_high_address_does_not_strand_low_gap() {
        let net = net29();
        let mut alloc = IpAllocator::new();
        // Claim the highest host; the cursor must NOT jump past it, so the lower
        // hosts stay allocatable (the old cursor-bump would exhaust the subnet).
        alloc.claim("inuse", Ipv4Addr::new(10, 0, 0, 6)).unwrap();
        assert_eq!(
            alloc.allocate("a", &net, gw()).unwrap(),
            Ipv4Addr::new(10, 0, 0, 2)
        );
        assert_eq!(
            alloc.allocate("b", &net, gw()).unwrap(),
            Ipv4Addr::new(10, 0, 0, 3)
        );
        // The claimed address is still held: the owner keeps it.
        assert_eq!(
            alloc.allocate("inuse", &net, gw()).unwrap(),
            Ipv4Addr::new(10, 0, 0, 6)
        );
    }

    #[test]
    fn reclaim_same_id_new_address_frees_old() {
        let net = net29();
        let mut alloc = IpAllocator::new();
        alloc.claim("inuse", Ipv4Addr::new(10, 0, 0, 4)).unwrap();
        // Re-claiming the same id to a new address must return the old one to
        // the free-list, not orphan it.
        alloc.claim("inuse", Ipv4Addr::new(10, 0, 0, 5)).unwrap();
        assert_eq!(
            alloc.allocate("inuse", &net, gw()).unwrap(),
            Ipv4Addr::new(10, 0, 0, 5)
        );
        assert_eq!(
            alloc.allocate("other", &net, gw()).unwrap(),
            Ipv4Addr::new(10, 0, 0, 4)
        );
    }
}
