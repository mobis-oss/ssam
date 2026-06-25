// Copyright 2026 Hyundai Mobis Co., Ltd.
// SPDX-License-Identifier: Apache-2.0

//! `NetworkActor` and its [`NetworkManager`] wrapper: the rsactor front-end
//! that serializes the single bridge network's allocator state and drives the
//! netavark 2.0 bridge/veth/NAT integration.
//!
//! The actor owns an [`IpAllocator`] plus the parsed subnet/gateway and
//! exposes four idempotent operations:
//! - [`CreateNetns`](messages::CreateNetns): create the persistent named
//!   network namespace (never destroys an existing one).
//! - [`Attach`](messages::Attach): run netavark `setup` to wire veth + IP +
//!   NAT into the namespace.
//! - [`Detach`](messages::Detach): delete the host veth (its container-side peer
//!   and per-container state go with it) and release the in-memory IP mark. The
//!   shared bridge/NAT and the namespace are kept.
//! - [`DestroyNetns`](messages::DestroyNetns): delete the named namespace.
//!
//! Only the netavark `setup` (which forks the `nft` binary) runs on
//! [`tokio::task::spawn_blocking`]; the remaining short netlink/mount syscalls
//! run inline. Either way each handler is serialized through this single actor
//! — the property that keeps the allocator state race-free.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr};
use std::os::fd::{AsFd as _, BorrowedFd};
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use ipnet::{IpNet, Ipv4Net};
use rsactor::{Actor, ActorRef, message_handlers};

use netavark::firewall::{FirewallDriver, get_supported_firewall_driver};
use netavark::network::core_utils::open_netlink_sockets;
use netavark::network::driver::{DriverInfo, NetworkDriver, get_network_driver};
use netavark::network::netlink_route::LinkID;
use netavark::network::types::{
    NamedPerNetworkOptions, Network, NetworkOptions, PerNetworkOptions, PortMapping, Subnet,
};

use crate::network::allocator::{self, IpAllocator};
use crate::network::{link, netns};
use crate::utils::actor_supervisor::{IgnoreOnFailure, SupervisedActor, spawn_with};

/// Nominal netavark config directory. With `rootless: true` netavark never
/// reads or writes this path (it skips all on-disk state persistence), so the
/// daemon does not create it; the value only fills the required `DriverInfo`
/// field.
const NETAVARK_CONFIG_DIR: &str = "/run/ssam/netavark";

/// Force the nftables firewall backend (firewalld/fwnone are not used).
const FIREWALL_DRIVER: &str = "nftables";

/// netavark per-network option pinning the host-side veth name. Without it the
/// bridge driver auto-generates a random host interface, breaking the module's
/// `ssam-<hash>` naming assumption used for idempotency and sysfs scans.
const OPTION_HOST_INTERFACE_NAME: &str = "host_interface_name";

pub(crate) mod messages {
    //! Plain message structs handled by [`super::NetworkActor`].

    use netavark::network::types::PortMapping;

    /// Create the persistent named network namespace for `pkg`.
    pub(crate) struct CreateNetns {
        pub pkg: String,
    }

    /// Wire veth + IP + NAT into `pkg`'s namespace, exposing the resolved
    /// container-side interface as `container_interface`.
    pub(crate) struct Attach {
        pub pkg: String,
        pub container_interface: String,
        pub port_mappings: Option<Vec<PortMapping>>,
    }

    /// Delete `pkg`'s host veth and release its IP mark, keeping its namespace.
    /// When `port_mappings` are present, a netavark teardown runs before the
    /// veth delete to drop the published-port DNAT rules.
    pub(crate) struct Detach {
        pub pkg: String,
        pub container_interface: String,
        pub port_mappings: Option<Vec<PortMapping>>,
    }

    /// Delete `pkg`'s named network namespace.
    pub(crate) struct DestroyNetns {
        pub pkg: String,
    }
}

/// Result of a successful [`messages::Attach`].
#[derive(Clone, Debug)]
pub struct NetworkHandle {
    /// Persistent path of the package's named network namespace.
    pub netns_path: PathBuf,
    /// Deterministic container IP assigned inside the subnet.
    pub container_ip: Ipv4Addr,
    /// Bridge gateway address.
    pub gateway_ip: Ipv4Addr,
    /// Subnet prefix length (CIDR suffix).
    pub prefix_len: u8,
    /// Container-side interface name (e.g. `eth0`).
    pub container_interface: String,
}

/// rsactor actor owning the single bridge network's allocator state.
#[derive(Debug, Actor)]
pub struct NetworkActor {
    bridge_name: String,
    subnet: Ipv4Net,
    gateway: Ipv4Addr,
    allocator: IpAllocator,
}

impl SupervisedActor for NetworkActor {
    type FailurePolicy = IgnoreOnFailure;
}

#[message_handlers]
impl NetworkActor {
    /// Build an actor instance from daemon network configuration.
    ///
    /// # Errors
    ///
    /// Returns an error if `config.bridge_name` is not a valid Linux interface
    /// name, if `config.subnet` is not a valid IPv4 CIDR, or if the gateway lies
    /// outside the subnet.
    pub fn new(config: &crate::configuration::NetworkConfig) -> anyhow::Result<Self> {
        validate_interface_name(&config.bridge_name)
            .with_context(|| format!("Invalid bridge_name {:?}", config.bridge_name))?;
        let subnet: Ipv4Net = config
            .subnet
            .parse()
            .with_context(|| format!("Invalid network subnet {:?}", config.subnet))?;
        anyhow::ensure!(
            subnet.contains(&config.gateway),
            "Gateway {} is not within subnet {}",
            config.gateway,
            subnet
        );
        if !link::exists(&config.bridge_name) {
            log::info!(
                "Bridge {} absent; it is created by netavark on first attach",
                config.bridge_name
            );
        }
        Ok(Self {
            bridge_name: config.bridge_name.clone(),
            subnet,
            gateway: config.gateway,
            allocator: IpAllocator::new(),
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

    /// Idempotently attach veth + IP + NAT for the package.
    ///
    /// If the host veth and the namespace are both already present, the existing
    /// wiring is reused and a handle is returned without re-running netavark. A
    /// veth left over from a destroyed namespace is removed and recreated.
    #[handler]
    async fn handle_attach(
        &mut self,
        msg: messages::Attach,
        _actor_ref: &ActorRef<Self>,
    ) -> anyhow::Result<NetworkHandle> {
        let messages::Attach {
            pkg,
            container_interface,
            port_mappings,
        } = msg;
        validate_interface_name(&container_interface).with_context(|| {
            format!("Invalid container network interface_name {container_interface:?}")
        })?;
        let veth = allocator::veth_host_name(&pkg);
        let ns_name = netns::netns_name(&pkg);
        let ns_path = netns::netns_path(&ns_name);

        // Idempotency check. The veth name is a full-length hash of the package
        // name, so a present veth belongs to this package; a foreign collision is
        // not a practical concern.
        if link::exists(&veth) {
            if ns_path.exists() {
                // Already attached: recompute the IP (idempotent) and return a
                // handle without re-running netavark.
                let ip = self.allocator.allocate(&pkg, &self.subnet, self.gateway)?;
                return Ok(self.handle_for(ns_path, ip, container_interface));
            }
            // Stale leftover: the namespace is gone but the veth remains. netavark
            // setup would fail with EEXIST, so remove it first and let a fresh
            // setup recreate it.
            if let Err(e) = link::delete(&veth) {
                log::warn!("Failed to remove stale veth {veth} before re-setup: {e}");
            }
        }

        // Fresh setup.
        let ip = self.allocator.allocate(&pkg, &self.subnet, self.gateway)?;

        let options = build_network_options(
            &pkg,
            &self.bridge_name,
            self.subnet,
            self.gateway,
            ip,
            &container_interface,
            port_mappings,
        );
        let netns_path = ns_path
            .to_str()
            .context("netns path is not valid UTF-8")?
            .to_owned();

        // netavark setup forks+execs the `nft` binary and performs multiple
        // netlink round-trips (tens to hundreds of ms). It runs on a blocking
        // thread so it never stalls other tasks on the shared tokio runtime.
        let setup_result =
            tokio::task::spawn_blocking(move || run_netavark_setup(&netns_path, &options))
                .await
                .context("netavark setup task failed to join")?;

        if let Err(e) = setup_result {
            // Roll back the in-memory mark; keep the namespace intact.
            self.allocator.release(&pkg);
            // netavark may have created the host veth before failing. Remove it so
            // the next attach is not fooled by a leftover veth into reporting the
            // half-built network as already attached.
            if let Err(del) = link::delete(&veth) {
                log::warn!("Failed to remove veth {veth} after netavark setup failure: {del}");
            }
            return Err(e);
        }

        Ok(self.handle_for(ns_path, ip, container_interface))
    }

    /// Idempotently delete the package's host veth, keeping its namespace.
    /// Best-effort and forgiving: always returns `Ok`.
    ///
    /// Non-empty `port_mappings` trigger a netavark teardown before the veth
    /// delete to drop the published-port DNAT rules; teardown errors are logged,
    /// not propagated.
    #[handler]
    async fn handle_detach(
        &mut self,
        msg: messages::Detach,
        _actor_ref: &ActorRef<Self>,
    ) -> anyhow::Result<()> {
        let messages::Detach {
            pkg,
            container_interface,
            port_mappings,
        } = msg;
        let veth = allocator::veth_host_name(&pkg);

        if port_mappings.as_ref().is_some_and(|m| !m.is_empty()) {
            let teardown_result = async {
                let ip = self.allocator.allocate(&pkg, &self.subnet, self.gateway)?;
                let options = build_network_options(
                    &pkg,
                    &self.bridge_name,
                    self.subnet,
                    self.gateway,
                    ip,
                    &container_interface,
                    port_mappings,
                );
                let ns_name = netns::netns_name(&pkg);
                let netns_path = netns::netns_path(&ns_name)
                    .to_str()
                    .context("netns path is not valid UTF-8")?
                    .to_owned();
                tokio::task::spawn_blocking(move || run_netavark_teardown(&netns_path, &options))
                    .await
                    .context("netavark teardown task failed to join")?
            }
            .await;
            if let Err(e) = teardown_result {
                log::warn!("Best-effort netavark teardown for {pkg} failed: {e}");
            }
        }

        // Release the IP mark only once the host veth is confirmed gone. link::delete
        // returns Ok when the link is already absent, so Ok means no container still
        // holds this address. On delete failure keep the mark: leaking it is safe,
        // but reusing it would let another package collide with the lingering veth.
        match link::delete(&veth) {
            Ok(()) => self.allocator.release(&pkg),
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

    /// Assemble a [`NetworkHandle`] from per-package data plus actor state.
    fn handle_for(
        &self,
        netns_path: PathBuf,
        container_ip: Ipv4Addr,
        container_interface: String,
    ) -> NetworkHandle {
        NetworkHandle {
            netns_path,
            container_ip,
            gateway_ip: self.gateway,
            prefix_len: self.subnet.prefix_len(),
            container_interface,
        }
    }
}

/// Validate a Linux network interface name against kernel `IFNAMSIZ` rules.
///
/// The name must be 1..=15 bytes and must not be `.`/`..` or contain `/`,
/// whitespace, or control characters. Rejecting bad names here makes a
/// misconfigured `bridge_name` fail at daemon startup rather than at the first
/// attach deep inside netavark.
///
/// # Errors
///
/// Returns an error describing the first violated constraint.
fn validate_interface_name(name: &str) -> anyhow::Result<()> {
    anyhow::ensure!(!name.is_empty(), "interface name is empty");
    anyhow::ensure!(
        name.len() <= 15,
        "interface name exceeds 15 bytes (IFNAMSIZ): {} bytes",
        name.len()
    );
    anyhow::ensure!(name != "." && name != "..", "interface name is '.' or '..'");
    anyhow::ensure!(
        !name
            .chars()
            .any(|c| c == '/' || c == ':' || c.is_whitespace() || c.is_control()),
        "interface name contains '/' ':' whitespace or a control character"
    );
    Ok(())
}

fn build_bridge_network(bridge_name: &str, subnet: Ipv4Net, gateway: Ipv4Addr) -> Network {
    Network {
        created: None,
        dns_enabled: false,
        driver: "bridge".to_owned(),
        id: bridge_name.to_owned(),
        internal: false,
        ipv6_enabled: false,
        name: bridge_name.to_owned(),
        network_interface: Some(bridge_name.to_owned()),
        options: None,
        ipam_options: None,
        subnets: Some(vec![Subnet {
            gateway: Some(IpAddr::V4(gateway)),
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

/// Assemble netavark's owned `NetworkOptions` for attaching `pkg` to the bridge.
fn build_network_options(
    pkg: &str,
    bridge_name: &str,
    subnet: Ipv4Net,
    gateway: Ipv4Addr,
    container_ip: Ipv4Addr,
    container_interface: &str,
    port_mappings: Option<Vec<PortMapping>>,
) -> NetworkOptions {
    let mut network_info = HashMap::new();
    network_info.insert(
        bridge_name.to_owned(),
        build_bridge_network(bridge_name, subnet, gateway),
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
        port_mappings,
        dns_servers: None,
    }
}

/// Run a blocking netavark bridge `setup` for the pre-built `options`.
///
/// Opens netlink sockets, brings up loopback inside the namespace at
/// `netns_path`, and invokes the bridge driver. Performs blocking netlink I/O
/// and MUST run on a blocking executor (`spawn_blocking`). Teardown is
/// intentionally not handled here: removing the host veth via
/// [`link::delete`] plus destroying the namespace cleans up all
/// per-container state, and the per-subnet NAT rule is shared.
///
/// # Errors
///
/// Returns an error if any netavark stage (firewall driver, netlink sockets,
/// driver construction/validation, setup) fails.
fn run_netavark_setup(netns_path: &str, options: &NetworkOptions) -> anyhow::Result<()> {
    let firewall = get_supported_firewall_driver(Some(FIREWALL_DRIVER.to_owned()))
        .map_err(|e| anyhow::anyhow!("netavark firewall driver: {e}"))?;

    // The returned File handles MUST stay alive for the whole call: the fds in
    // DriverInfo borrow from them.
    let (mut hostns, mut netns) = open_netlink_sockets(netns_path)
        .map_err(|e| anyhow::anyhow!("netavark open netlink sockets for {netns_path}: {e}"))?;

    // Bring loopback (ifindex 1) up inside the container namespace.
    netns
        .netlink
        .set_up(LinkID::ID(1))
        .map_err(|e| anyhow::anyhow!("netavark set loopback up: {e}"))?;

    let driver = build_netavark_driver(
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

/// Run a blocking netavark bridge `teardown` for the pre-built `options`.
///
/// Mirrors [`run_netavark_setup`] with fresh netlink sockets and an identical
/// [`DriverInfo`]; removes the published-port DNAT rules carried in
/// `options.port_mappings`. Performs blocking netlink I/O and MUST run on a
/// blocking executor (`spawn_blocking`).
///
/// # Errors
///
/// Returns an error if any netavark stage (firewall driver, netlink sockets,
/// driver construction/validation, teardown) fails.
fn run_netavark_teardown(netns_path: &str, options: &NetworkOptions) -> anyhow::Result<()> {
    let firewall = get_supported_firewall_driver(Some(FIREWALL_DRIVER.to_owned()))
        .map_err(|e| anyhow::anyhow!("netavark firewall driver: {e}"))?;

    let (mut hostns, mut netns) = open_netlink_sockets(netns_path)
        .map_err(|e| anyhow::anyhow!("netavark open netlink sockets for {netns_path}: {e}"))?;

    let driver = build_netavark_driver(
        netns_path,
        options,
        firewall.as_ref(),
        hostns.file.as_fd(),
        netns.file.as_fd(),
    )?;

    let sockets = (&mut hostns.netlink, &mut netns.netlink);
    driver
        .teardown(sockets)
        .map_err(|e| anyhow::anyhow!("netavark teardown: {e}"))
}

fn build_netavark_driver<'a>(
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
    /// Returns an error if the actor cannot be constructed (invalid subnet).
    pub fn new(config: &crate::configuration::NetworkConfig) -> anyhow::Result<Self> {
        let actor = NetworkActor::new(config)?;
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

    /// Idempotently attach veth + IP + NAT for the package.
    ///
    /// # Errors
    ///
    /// Returns an error if the actor has stopped, an IP collision is detected,
    /// the veth is foreign-owned, or netavark setup fails.
    pub async fn attach(
        &self,
        pkg: &str,
        container_interface: &str,
        port_mappings: Option<Vec<PortMapping>>,
    ) -> anyhow::Result<NetworkHandle> {
        self.actor
            .ask(messages::Attach {
                pkg: pkg.to_owned(),
                container_interface: container_interface.to_owned(),
                port_mappings,
            })
            .await
            .context("NetworkActor has died?")?
    }

    /// Idempotently detach the package's host veth (keeps the namespace).
    ///
    /// Non-empty `port_mappings` trigger a netavark teardown before the veth
    /// delete; the same `container_interface` passed to [`attach`](Self::attach)
    /// must be supplied so netavark can rebuild the teardown options.
    ///
    /// # Errors
    ///
    /// Returns an error only if the actor has stopped; teardown itself is
    /// best-effort and always reports success.
    pub async fn detach(
        &self,
        pkg: &str,
        container_interface: &str,
        port_mappings: Option<Vec<PortMapping>>,
    ) -> anyhow::Result<()> {
        self.actor
            .ask(messages::Detach {
                pkg: pkg.to_owned(),
                container_interface: container_interface.to_owned(),
                port_mappings,
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

    fn config(subnet: &str) -> crate::configuration::NetworkConfig {
        crate::configuration::NetworkConfig {
            bridge_enabled: true,
            bridge_name: "ssam-br0".to_owned(),
            subnet: subnet.to_owned(),
            gateway: Ipv4Addr::new(172, 20, 0, 1),
        }
    }

    #[test]
    fn new_parses_valid_subnet() {
        let actor = NetworkActor::new(&config("172.20.0.0/16")).unwrap();
        assert_eq!(actor.subnet.prefix_len(), 16);
        assert_eq!(actor.gateway, Ipv4Addr::new(172, 20, 0, 1));
    }

    #[test]
    fn new_errors_on_bad_subnet() {
        let err = NetworkActor::new(&config("not-a-subnet")).unwrap_err();
        assert!(err.to_string().contains("subnet"));
    }

    #[test]
    fn new_errors_on_gateway_outside_subnet() {
        let mut cfg = config("172.20.0.0/16");
        cfg.gateway = Ipv4Addr::new(10, 0, 0, 1);
        let err = NetworkActor::new(&cfg).unwrap_err();
        assert!(err.to_string().contains("not within subnet"));
    }

    #[test]
    fn new_errors_on_bad_bridge_name() {
        let mut cfg = config("172.20.0.0/16");
        cfg.bridge_name = "this-name-is-way-too-long".to_owned();
        let err = NetworkActor::new(&cfg).unwrap_err();
        assert!(err.to_string().contains("bridge_name"));
    }

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

    fn port_mapping() -> PortMapping {
        PortMapping {
            container_port: 80,
            host_ip: "0.0.0.0".to_owned(),
            host_port: 8080,
            protocol: "tcp".to_owned(),
            range: 1,
        }
    }

    #[test]
    fn build_network_options_carries_port_mappings() {
        let opts = build_network_options(
            "pkg-a",
            "ssam-br0",
            "172.20.0.0/16".parse().unwrap(),
            Ipv4Addr::new(172, 20, 0, 1),
            Ipv4Addr::new(172, 20, 0, 2),
            "eth0",
            Some(vec![port_mapping()]),
        );
        assert_eq!(opts.port_mappings, Some(vec![port_mapping()]));
    }

    #[test]
    fn build_network_options_omits_absent_port_mappings() {
        let opts = build_network_options(
            "pkg-a",
            "ssam-br0",
            "172.20.0.0/16".parse().unwrap(),
            Ipv4Addr::new(172, 20, 0, 1),
            Ipv4Addr::new(172, 20, 0, 2),
            "eth0",
            None,
        );
        assert_eq!(opts.port_mappings, None);
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
