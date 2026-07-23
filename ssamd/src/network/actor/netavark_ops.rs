// Copyright 2026 Hyundai Mobis Co., Ltd.
// SPDX-License-Identifier: Apache-2.0

//! netavark 2.0 glue for [`super::NetworkActor`]: builds `NetworkOptions` and
//! runs the blocking bridge `setup`, kept apart from the actor's state logic.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr};
use std::os::fd::{AsFd as _, BorrowedFd};
use std::path::Path;

use ipnet::{IpNet, Ipv4Net};

use netavark::firewall::{FirewallDriver, get_supported_firewall_driver};
use netavark::network::core_utils::open_netlink_sockets;
use netavark::network::driver::{DriverInfo, NetworkDriver, get_network_driver};
use netavark::network::netlink_route::LinkID;
use netavark::network::types::{
    NamedPerNetworkOptions, Network, NetworkOptions, PerNetworkOptions, Subnet,
};

use crate::network::allocator;

/// Nominal netavark config directory. With `rootless: true` netavark never
/// reads or writes this path (it skips all on-disk state persistence), so the
/// daemon does not create it; the value only fills the required `DriverInfo`
/// field.
const NETAVARK_CONFIG_DIR: &str = "/run/ssam/netavark";

/// No firewall backend. ssam bridges are always netavark-internal (no
/// masquerade, published ports, or NAT), so netavark installs zero rules and
/// the `nft` binary is never needed. `none` (`Fwnone`) keeps that true even if
/// netavark's internal short-circuit ever changes.
const FIREWALL_DRIVER: &str = "none";

/// netavark per-network option pinning the host-side veth name. Without it the
/// bridge driver auto-generates a random host interface, breaking the module's
/// `ssam-<hash>` naming assumption used for idempotency and sysfs scans.
const OPTION_HOST_INTERFACE_NAME: &str = "host_interface_name";

/// Validate a network interface name against kernel `IFNAMSIZ` rules, so a bad
/// `interface_name` fails here instead of deep inside netavark.
///
/// # Errors
///
/// Returns an error describing the first violated constraint.
pub(super) fn validate_interface_name(name: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        (1..=15).contains(&name.len()),
        "interface name must be 1..=15 bytes (IFNAMSIZ), got {}",
        name.len()
    );
    anyhow::ensure!(!matches!(name, "." | ".."), "interface name is '.' or '..'");
    anyhow::ensure!(
        !name.contains(|c: char| c == '/' || c == ':' || c.is_whitespace() || c.is_control()),
        "interface name contains '/' ':' whitespace or a control character"
    );
    Ok(())
}

/// Build netavark's `Network` for a bridge. `internal = true` with no subnet
/// gateway yields an isolated bridge (no gateway IP, default route, or masquerade).
fn build_bridge_network(bridge_name: &str, subnet: Ipv4Net) -> Network {
    Network {
        created: None,
        dns_enabled: false,
        driver: "bridge".to_owned(),
        // id/name/network_interface all = bridge name: netavark keys its firewall
        // hash on these and never re-validates a reused bridge's subnet.
        id: bridge_name.to_owned(),
        internal: true,
        ipv6_enabled: false,
        name: bridge_name.to_owned(),
        network_interface: Some(bridge_name.to_owned()),
        options: None,
        ipam_options: None,
        subnets: Some(vec![Subnet {
            gateway: None,
            lease_range: None,
            subnet: IpNet::V4(subnet),
        }]),
        routes: None,
        network_dns_servers: None,
        labels: None,
    }
}

fn build_per_network_opts(
    pkg: &str,
    container_ip: Ipv4Addr,
    container_interface: &str,
) -> PerNetworkOptions {
    let mut options = HashMap::new();
    options.insert(
        OPTION_HOST_INTERFACE_NAME.to_owned(),
        allocator::veth_host_name(pkg),
    );
    PerNetworkOptions {
        aliases: None,
        interface_name: container_interface.to_owned(),
        static_ips: Some(vec![IpAddr::V4(container_ip)]),
        static_mac: None,
        options: Some(options),
    }
}

/// Assemble netavark's owned `NetworkOptions` for attaching `pkg` to `bridge_name`.
pub(super) fn build_network_options(
    pkg: &str,
    bridge_name: &str,
    subnet: Ipv4Net,
    container_ip: Ipv4Addr,
    container_interface: &str,
) -> NetworkOptions {
    let mut network_info = HashMap::new();
    network_info.insert(
        bridge_name.to_owned(),
        build_bridge_network(bridge_name, subnet),
    );
    NetworkOptions {
        container_id: pkg.to_owned(),
        container_name: pkg.to_owned(),
        container_hostname: None,
        networks: vec![NamedPerNetworkOptions {
            name: bridge_name.to_owned(),
            opts: build_per_network_opts(pkg, container_ip, container_interface),
        }],
        network_info,
        port_mappings: None,
        dns_servers: None,
    }
}

/// Run the blocking netavark bridge `setup`. MUST run on a blocking executor.
///
/// # Errors
///
/// Returns an error if any netavark stage (firewall driver, netlink sockets,
/// driver construction/validation, setup) fails.
pub(super) fn run_setup(netns_path: &str, options: &NetworkOptions) -> anyhow::Result<()> {
    let firewall = get_supported_firewall_driver(Some(FIREWALL_DRIVER.to_owned()))
        .map_err(|e| anyhow::anyhow!("netavark firewall driver: {e}"))?;

    // Socket fds in DriverInfo borrow from these File handles — keep alive past the call.
    let (mut hostns, mut netns) = open_netlink_sockets(netns_path)
        .map_err(|e| anyhow::anyhow!("netavark open netlink sockets for {netns_path}: {e}"))?;

    netns
        .netlink
        .set_up(LinkID::ID(1))
        .map_err(|e| anyhow::anyhow!("netavark set loopback up: {e}"))?;

    let driver = build_driver(
        netns_path,
        options,
        firewall.as_ref(),
        hostns.file.as_fd(),
        netns.file.as_fd(),
    )?;

    let sockets = (&mut hostns.netlink, &mut netns.netlink);
    driver
        .setup(sockets)
        .map(|_status| ())
        .map_err(|e| anyhow::anyhow!("netavark setup: {e}"))
}

fn build_driver<'a>(
    netns_path: &'a str,
    options: &'a NetworkOptions,
    firewall: &'a dyn FirewallDriver,
    hostns_fd: BorrowedFd<'a>,
    netns_fd: BorrowedFd<'a>,
) -> anyhow::Result<Box<dyn NetworkDriver + 'a>> {
    let named = &options.networks[0];
    let network = &options.network_info[&named.name];
    let info = DriverInfo {
        firewall,
        container_id: &options.container_id,
        container_name: &options.container_name,
        container_dns_servers: &options.dns_servers,
        netns_host: hostns_fd,
        netns_container: netns_fd,
        netns_path,
        network,
        per_network_opts: &named.opts,
        port_mappings: &options.port_mappings,
        // 53 is a sentinel: netavark adds a DNS DNAT redirect for every gateway
        // nameserver whenever dns_port != 53 (independent of dns_enabled), so any
        // other value — including 0 — would inject a bogus port-0 redirect rule.
        dns_port: 53,
        config_dir: Path::new(NETAVARK_CONFIG_DIR),
        // rootless suppresses netavark's only on-disk writes (firewall-state
        // files under config_dir and the /run/sysctl.d advisory file) while the
        // bridge/veth/NAT and sysctls are still applied via netlink/sysctl. This
        // keeps the daemon free of any writable-filesystem dependency. Do NOT set
        // this to false: it reintroduces on-disk state the target may not allow.
        rootless: true,
        container_hostname: &options.container_hostname,
    };
    let mut driver =
        get_network_driver(info, &None).map_err(|e| anyhow::anyhow!("netavark get driver: {e}"))?;
    driver
        .validate()
        .map_err(|e| anyhow::anyhow!("netavark validate: {e}"))?;
    Ok(driver)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_interface_name_accepts_and_rejects() {
        assert!(validate_interface_name("ssam-br0").is_ok());
        assert!(validate_interface_name("eth0").is_ok());
        assert!(validate_interface_name("").is_err());
        assert!(validate_interface_name("0123456789abcdef").is_err()); // 16 bytes
        assert!(validate_interface_name("eth/0").is_err());
        assert!(validate_interface_name("eth 0").is_err());
        assert!(validate_interface_name("..").is_err());
    }

    const BR: &str = "ssb-0123456789";

    #[test]
    fn build_network_options_has_no_gateway_and_is_always_internal() {
        let opts = build_network_options(
            "pkg-a",
            BR,
            "172.20.0.0/29".parse().unwrap(),
            Ipv4Addr::new(172, 20, 0, 1),
            "eth0",
        );
        let net = &opts.network_info[BR];
        assert!(net.internal);
        assert_eq!(net.subnets.as_ref().unwrap()[0].gateway, None);
    }
}
