// Copyright 2026 Hyundai Mobis Co., Ltd.
// SPDX-License-Identifier: Apache-2.0

//! Boot-time restore scan for [`super::NetworkActor`]: reads live bridge/subnet/IP
//! state from host truth and enumerates `ssam-*`/`ssb-*` links for the orphan
//! sweep. Stateless, driven from `on_start`.

use std::net::{IpAddr, Ipv4Addr};
use std::path::Path;

use anyhow::Context as _;
use ipnet::Ipv4Net;

use netavark::network::core_utils::open_netlink_sockets;
use netlink_packet_route::address::AddressAttribute;

use crate::network::{link, netns};

/// A live container's veth on its bridge, with the bridge's observed subnet.
pub(super) struct RestoredAttach {
    pub(super) br: String,
    pub(super) subnet: Ipv4Net,
    pub(super) veth: String,
    pub(super) ip: Ipv4Addr,
}

/// One entry of the restore plan handed back to [`super::NetworkActor`].
pub(super) enum RestoreEntry {
    /// Full recovery: claim the bridge's subnet and the container's live IP.
    Attach(RestoredAttach),
    /// A live container holds this in-pool IP but couldn't be fully restored;
    /// reserve it so the address is never reissued. `master` is the known
    /// `ssb-*` bridge, or `None` if the lookup itself failed.
    ReserveIp {
        ip: Ipv4Addr,
        veth: String,
        master: Option<String>,
    },
}

/// Scan the netns directory and build the restore plan. Must run on the
/// dedicated restore thread — `read_container_ipv4` calls `setns`.
pub(super) fn collect_restore_plan(pool: Ipv4Net, subnet_prefix: u8) -> Vec<RestoreEntry> {
    let dir = netns::netns_dir();
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(e) => {
            log::warn!(
                "network restore: cannot read netns dir {}: {e}",
                dir.display()
            );
            return Vec::new();
        }
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            if !name.starts_with("ssam-") {
                return None;
            }
            match build_restore_entry(&entry.path(), &name, pool, subnet_prefix) {
                Ok(entry) => entry,
                Err(e) => {
                    log::warn!("network restore: skipping {name}: {e}");
                    None
                }
            }
        })
        .collect()
}

/// `Ok(None)` when the veth is not ours (no in-pool address, or a foreign/absent
/// master).
fn build_restore_entry(
    netns_path: &Path,
    name: &str,
    pool: Ipv4Net,
    subnet_prefix: u8,
) -> anyhow::Result<Option<RestoreEntry>> {
    let Some(container) = read_container_ipv4(netns_path, pool)? else {
        return Ok(None);
    };
    // An `ssb-*` master is ours to restore. A different/absent master is not our
    // bridge, so leave its in-pool overlap to the operator. A lookup error can't
    // prove the veth is foreign, so fail closed and reserve its slot.
    let master = match link::enslaving_bridge(name) {
        Ok(Some(master)) if master.starts_with("ssb-") => master,
        Ok(_) => {
            log::warn!("network restore: veth {name} not on an ssb- bridge; skipping");
            return Ok(None);
        }
        Err(e) => {
            log::warn!(
                "network restore: veth {name} master lookup failed: {e}; reserving its slot"
            );
            return Ok(Some(RestoreEntry::ReserveIp {
                ip: container.addr(),
                veth: name.to_owned(),
                master: None,
            }));
        }
    };
    let ip = container.addr();
    if container.prefix_len() != subnet_prefix {
        log::warn!(
            "network restore: container {name} address /{} disagrees with pool slot /{subnet_prefix}; reserving its slot",
            container.prefix_len()
        );
        return Ok(Some(RestoreEntry::ReserveIp {
            ip,
            veth: name.to_owned(),
            master: Some(master),
        }));
    }
    Ok(Some(RestoreEntry::Attach(RestoredAttach {
        br: master,
        subnet: container.trunc(),
        veth: name.to_owned(),
        ip,
    })))
}

/// Map one address attribute to the container's own in-pool IPv4, or `None` for
/// a non-IPv4 or out-of-pool address.
fn container_ipv4_in_pool(attr: &AddressAttribute, prefix: u8, pool: Ipv4Net) -> Option<Ipv4Net> {
    let AddressAttribute::Address(IpAddr::V4(v4)) = attr else {
        return None;
    };
    let net = Ipv4Net::new(*v4, prefix).ok()?;
    pool.contains(v4).then_some(net)
}

/// The container's own in-pool IPv4 in `netns_path` (a ssam container has
/// exactly one). `Ok(None)` when there is nothing to restore.
///
/// Does blocking netlink I/O and a `setns`, so it MUST run on the restore thread.
///
/// # Errors
///
/// Errors if the path is not valid UTF-8, the netlink sockets cannot be opened,
/// or the address dump fails.
fn read_container_ipv4(netns_path: &Path, pool: Ipv4Net) -> anyhow::Result<Option<Ipv4Net>> {
    let netns_path = netns_path
        .to_str()
        .context("netns path is not valid UTF-8")?;

    // Socket fds borrow from these File handles — keep alive past the dump.
    let (_hostns, mut netns) = open_netlink_sockets(netns_path)
        .map_err(|e| anyhow::anyhow!("open netlink sockets for {netns_path}: {e}"))?;

    let addresses = netns
        .netlink
        .dump_addresses(None)
        .map_err(|e| anyhow::anyhow!("dump addresses in {netns_path}: {e}"))?;

    Ok(addresses.into_iter().find_map(|addr| {
        let prefix = addr.header.prefix_len;
        addr.attributes
            .into_iter()
            .find_map(|attr| container_ipv4_in_pool(&attr, prefix, pool))
    }))
}

/// Enumerate host `ssam-*` veths and `ssb-*` bridges from `/sys/class/net`,
/// returned as `(veths, bridges)`. A read error yields empty lists (logged).
pub(super) fn enumerate_managed_links() -> (Vec<String>, Vec<String>) {
    let Ok(entries) = std::fs::read_dir(link::SYS_CLASS_NET)
        .inspect_err(|e| log::warn!("network restore: cannot read {}: {e}", link::SYS_CLASS_NET))
    else {
        return (Vec::new(), Vec::new());
    };
    entries
        .flatten()
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| name.starts_with("ssam-") || name.starts_with("ssb-"))
        .partition(|name| name.starts_with("ssam-"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn container_ipv4_in_pool_accepts_in_pool_rejects_out_of_pool() {
        let pool: Ipv4Net = "172.20.0.0/16".parse().unwrap();
        let in_pool = AddressAttribute::Address(IpAddr::V4(Ipv4Addr::new(172, 20, 0, 5)));
        assert_eq!(
            container_ipv4_in_pool(&in_pool, 29, pool),
            Some("172.20.0.5/29".parse().unwrap())
        );

        let out_of_pool = AddressAttribute::Address(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 5)));
        assert_eq!(container_ipv4_in_pool(&out_of_pool, 29, pool), None);
    }
}
