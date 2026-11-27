//! Which addresses may reach the phone listener (REQ-036): the local network
//! only. Pure: the caller passes this machine's IPv4 interfaces.
use std::net::{IpAddr, Ipv4Addr};

/// One local IPv4 interface: its address and netmask.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Interface {
    pub ip: Ipv4Addr,
    pub netmask: Ipv4Addr,
}

fn private(ip: Ipv4Addr) -> bool {
    ip.is_private() || ip.is_link_local()
}

/// A private or link-local IPv4 source on the subnet of a local interface.
/// Loopback is refused: the laptop has the admin page.
pub fn is_lan_source(source: IpAddr, interfaces: &[Interface]) -> bool {
    let ip = match source {
        IpAddr::V4(ip) => ip,
        IpAddr::V6(ip) => match ip.to_ipv4_mapped() {
            Some(ip) => ip,
            None => return false,
        },
    };
    if !private(ip) {
        return false;
    }
    interfaces.iter().any(|interface| {
        let mask = u32::from(interface.netmask);
        private(interface.ip) && mask != 0 && u32::from(interface.ip) & mask == u32::from(ip) & mask
    })
}

/// The addresses a phone can use to reach this bridge: private and
/// link-local IPv4 interface addresses, the one of the default route first
/// (`preferred`), then other private ones, then link-local ones; no loopback.
pub fn lan_addresses(interfaces: &[Interface], preferred: Option<Ipv4Addr>) -> Vec<Ipv4Addr> {
    let mut addresses: Vec<Ipv4Addr> = Vec::new();
    for ip in interfaces.iter().map(|i| i.ip) {
        if private(ip) && !ip.is_loopback() && !addresses.contains(&ip) {
            addresses.push(ip);
        }
    }
    addresses.sort_by_key(|ip| match ip {
        ip if Some(*ip) == preferred => 0,
        ip if ip.is_private() => 1,
        _ => 2,
    });
    addresses
}

/// `sb-<label>.local`, in lower case and within the 63-byte DNS label limit.
pub fn host_name(label: &str) -> String {
    let mut name = format!("sb-{}", label.to_ascii_lowercase());
    name.truncate(63);
    format!("{}.local", name.trim_end_matches('-'))
}

/// The names a request may use for this listener: its LAN addresses and its
/// `.local` name.
pub fn site_names(addresses: &[Ipv4Addr], host: &str) -> Vec<String> {
    addresses
        .iter()
        .map(Ipv4Addr::to_string)
        .chain(std::iter::once(host.to_owned()))
        .collect()
}

/// The request's `Host` names this listener (DNS rebinding): one of `names`
/// with the listener's port.
pub fn host_allowed(header: Option<&str>, names: &[String], port: u16) -> bool {
    header.is_some_and(|value| {
        names
            .iter()
            .any(|name| value.eq_ignore_ascii_case(&format!("{name}:{port}")))
    })
}

/// A browser's `Origin`, when sent, is this listener under one of `names`.
pub fn origin_allowed(header: Option<&str>, names: &[String], port: u16) -> bool {
    header.is_none_or(|value| {
        names
            .iter()
            .any(|name| value.eq_ignore_ascii_case(&format!("https://{name}:{port}")))
    })
}

/// The host part of an allowed `Host` header, in lower case: what the phone
/// signs (`location.hostname`).
pub fn request_host(header: &str) -> String {
    header
        .rsplit_once(':')
        .map_or(header, |(host, _)| host)
        .to_ascii_lowercase()
}
