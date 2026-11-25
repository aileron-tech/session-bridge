//! Zeroconf (mDNS + DNS-SD): announce this bridge and track the others.
//! The announcement carries only the protocol version and the key
//! fingerprint; it is a hint, never an identity. The bridge listens on IPv4,
//! so it announces and connects over IPv4 only.
use super::model::{DiscoveredHost, Fingerprint, HostLabel, order_addresses};
use anyhow::Result;
use mdns_sd::{IfKind, ServiceDaemon, ServiceEvent, ServiceInfo};
use std::{
    collections::BTreeMap,
    net::{IpAddr, SocketAddr},
    sync::{Arc, Mutex},
};
use tokio::task::JoinHandle;

pub const SERVICE: &str = "_session-bridge._tcp.local.";

pub struct Announcer {
    daemon: ServiceDaemon,
    fullname: String,
    hosts: Arc<Mutex<BTreeMap<String, DiscoveredHost>>>,
    browser: JoinHandle<()>,
}
impl Announcer {
    pub fn start(label: &HostLabel, fingerprint: &Fingerprint, port: u16) -> Result<Self> {
        let daemon = ServiceDaemon::new()?;
        daemon.disable_interface(IfKind::IPv6)?;
        let properties = [("v", "1".to_string()), ("fp", fingerprint.to_string())];
        // A host name of its own, so the record never competes with the OS's.
        let info = ServiceInfo::new(
            SERVICE,
            label.as_str(),
            &format!("sb-{label}.local."),
            "",
            port,
            &properties[..],
        )?
        .enable_addr_auto();
        let fullname = info.get_fullname().to_owned();
        daemon.register(info)?;
        let events = daemon.browse(SERVICE)?;
        let hosts = Arc::new(Mutex::new(BTreeMap::new()));
        let seen = hosts.clone();
        let browser = tokio::spawn(async move {
            while let Ok(event) = events.recv_async().await {
                let mut hosts = match seen.lock() {
                    Ok(hosts) => hosts,
                    Err(poisoned) => poisoned.into_inner(),
                };
                match event {
                    ServiceEvent::ServiceResolved(service) => {
                        let addresses = service
                            .get_addresses()
                            .iter()
                            .map(|ip| ip.to_ip_addr())
                            .collect();
                        match parse_announcement(
                            &service.fullname,
                            service.get_property_val_str("fp"),
                            addresses,
                            service.port,
                        ) {
                            Some(host) => {
                                hosts.insert(service.fullname.clone(), host);
                            }
                            None => eprintln!(
                                "Ignored malformed bridge announcement {}",
                                service.fullname
                            ),
                        }
                    }
                    ServiceEvent::ServiceRemoved(_, fullname) => {
                        hosts.remove(&fullname);
                    }
                    _ => {}
                }
            }
        });
        Ok(Self {
            daemon,
            fullname,
            hosts,
            browser,
        })
    }
    pub fn hosts(&self) -> Vec<DiscoveredHost> {
        match self.hosts.lock() {
            Ok(hosts) => hosts.values().cloned().collect(),
            Err(poisoned) => poisoned.into_inner().values().cloned().collect(),
        }
    }
    /// Withdraws the announcement and stops browsing.
    pub fn stop(self) {
        if let Err(e) = self.daemon.unregister(&self.fullname) {
            eprintln!("Could not withdraw the network announcement: {e}");
        }
        if let Err(e) = self.daemon.shutdown() {
            eprintln!("Could not stop network discovery: {e}");
        }
        self.browser.abort();
    }
}

/// A bridge from a resolved announcement: instance name, `fp` TXT value,
/// addresses and port. `None` when the name or fingerprint is invalid or no
/// usable IPv4 address is known yet, so a host is listed only once it is
/// reachable. Loopback addresses (announced on the loopback interface) would
/// lead another laptop back to itself and are dropped.
pub fn parse_announcement(
    fullname: &str,
    fingerprint: Option<&str>,
    addresses: Vec<IpAddr>,
    port: u16,
) -> Option<DiscoveredHost> {
    let instance = fullname.strip_suffix(&format!(".{SERVICE}"))?;
    let addresses: Vec<SocketAddr> = addresses
        .into_iter()
        .filter(|ip| ip.is_ipv4() && !ip.is_loopback())
        .map(|ip| SocketAddr::new(ip, port))
        .collect();
    (!addresses.is_empty()).then_some(())?;
    Some(DiscoveredHost {
        label: HostLabel::parse(instance).ok()?,
        fingerprint: Fingerprint::parse(fingerprint?).ok()?,
        addresses: order_addresses(addresses),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /* REQ-017: Announcements yield a host only with a valid name, fingerprint
    and an IPv4 address; IPv6 addresses, which the listener does not serve, are dropped. */
    #[test]
    fn parses_announcements() {
        let fp = Fingerprint::of_public_key(b"b");
        let v4: IpAddr = "192.168.1.20".parse().unwrap();
        let v6: IpAddr = "fe80::1".parse().unwrap();
        let loopback: IpAddr = "127.0.0.1".parse().unwrap();
        let host = parse_announcement(
            &format!("laptop-b.{SERVICE}"),
            Some(&fp.to_string()),
            vec![loopback, v6, v4],
            47820,
        )
        .unwrap();
        assert_eq!(host.label.as_str(), "laptop-b");
        assert_eq!(host.fingerprint, fp);
        assert_eq!(host.addresses, [SocketAddr::new(v4, 47820)]);
        let valid = fp.to_string();
        assert!(
            parse_announcement(
                &format!("laptop-b.{SERVICE}"),
                Some(&valid),
                vec![v6, loopback],
                1
            )
            .is_none(),
            "only IPv6 and loopback known yet"
        );
        for (name, key) in [
            (format!("laptop-b.{SERVICE}"), None),
            (format!("laptop-b.{SERVICE}"), Some("bad")),
            (
                "laptop-b._other._tcp.local.".to_string(),
                Some(valid.as_str()),
            ),
            (format!("bad name.{SERVICE}"), Some(valid.as_str())),
        ] {
            assert!(
                parse_announcement(&name, key, vec![v4], 1).is_none(),
                "{name}"
            );
        }
    }
}
