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

/// The address a phone uses to download the certificate profile: the first
/// private, non-link-local interface address.
pub fn profile_address(interfaces: &[Interface]) -> Option<Ipv4Addr> {
    interfaces
        .iter()
        .map(|i| i.ip)
        .find(|ip| ip.is_private())
        .or_else(|| interfaces.iter().map(|i| i.ip).find(|ip| private(*ip)))
}

/// `sb-<label>.local`, in lower case and within the 63-byte DNS label limit.
pub fn host_name(label: &str) -> String {
    let mut name = format!("sb-{}", label.to_ascii_lowercase());
    name.truncate(63);
    format!("{}.local", name.trim_end_matches('-'))
}

/// The request's `Host` names this listener (DNS rebinding).
pub fn host_matches(header: Option<&str>, host: &str, port: u16) -> bool {
    header.is_some_and(|value| value.eq_ignore_ascii_case(&format!("{host}:{port}")))
}
