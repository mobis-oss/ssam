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
//! The allocation state lives only in memory and is rebuilt empty on every
//! daemon start; there is no persistent lease. This is sound because the daemon
//! is the sole executor of containers, so nothing is attached when it starts.

use std::collections::{BTreeSet, HashMap};
use std::net::Ipv4Addr;

use ipnet::Ipv4Net;

/// Host-side veth interface name for a package (15 chars, `IFNAMSIZ - 1`).
#[must_use]
pub fn veth_host_name(pkg: &str) -> String {
    format!("ssam-{}", crate::network::pkg_hash10(pkg))
}

/// In-memory tracker of which package currently holds which address.
///
/// Allocation returns the lowest free host address without scanning the whole
/// subnet: a `freed` set holds released addresses (all below `next`) and is
/// drained lowest-first, and only once it is empty does the `next` high-water
/// cursor advance into never-yet-assigned space. Every assigned address is thus
/// either tracked in `attached` or recorded in `freed`, so the lowest free
/// address is `min(freed)` when non-empty and `next` otherwise.
#[derive(Debug, Default)]
pub struct IpAllocator {
    attached: HashMap<String, Ipv4Addr>,
    /// Released host addresses (as `u32`), all below `next`, sorted so the
    /// lowest is reused first.
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

    /// Allocate (or return the existing) IP for `pkg` as the lowest free host
    /// address in `subnet`.
    ///
    /// Idempotent for a repeated `pkg`. Allocation guarantees no two tracked
    /// packages share an address.
    ///
    /// # Errors
    ///
    /// Returns an error if the subnet has no free host address left.
    pub fn allocate(
        &mut self,
        pkg: &str,
        subnet: &Ipv4Net,
        gateway: Ipv4Addr,
    ) -> anyhow::Result<Ipv4Addr> {
        if let Some(existing) = self.attached.get(pkg) {
            return Ok(*existing);
        }

        let ip = self.next_free(*subnet, gateway)?;
        self.attached.insert(pkg.to_owned(), ip);
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

        // The never-assigned range [cursor, broadcast) excludes only the gateway,
        // which the cursor crosses once, so a single skip replaces a full scan.
        let mut candidate = self.next.unwrap_or(first_host);
        if candidate == gateway_u32 {
            // Skip the gateway. checked_add guards the (unrealistic) /0-subnet,
            // gateway-at-top-of-space case where the bump would overflow u32
            // before the bound check below; on overflow fall through to exhaustion.
            candidate = candidate.checked_add(1).unwrap_or(broadcast);
        }
        anyhow::ensure!(
            candidate < broadcast,
            "Subnet {subnet} has no free host address (gateway {gateway})"
        );
        self.next = Some(candidate + 1);
        Ok(Ipv4Addr::from(candidate))
    }

    /// Remove the attachment for `pkg`, returning its address to the free-list.
    /// Absent entries are a no-op.
    pub fn release(&mut self, pkg: &str) {
        if let Some(ip) = self.attached.remove(pkg) {
            self.freed.insert(u32::from(ip));
        }
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
}
