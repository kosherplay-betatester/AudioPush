//! UDP transport and peer discovery.

pub mod discovery;
pub mod pair;
pub mod protocol;
pub mod receiver;
pub mod scan;
pub mod sender;

use std::net::Ipv4Addr;

/// This machine's name, as the receiver advertises it.
pub fn hostname() -> String {
    std::env::var("COMPUTERNAME")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "PC".to_string())
}

/// An IPv4 address on this machine, with the interface it belongs to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalAddress {
    pub interface: String,
    pub ip: Ipv4Addr,
    pub netmask: Ipv4Addr,
}

impl LocalAddress {
    /// The directed broadcast address for this interface's subnet.
    pub fn broadcast(&self) -> Ipv4Addr {
        let ip = u32::from(self.ip);
        let mask = u32::from(self.netmask);
        Ipv4Addr::from(ip | !mask)
    }

    /// Number of addresses in this subnet.
    pub fn subnet_size(&self) -> u64 {
        (!u32::from(self.netmask)) as u64 + 1
    }

    /// Iterates the usable host addresses of this subnet.
    pub fn hosts(&self) -> impl Iterator<Item = Ipv4Addr> + use<> {
        let ip = u32::from(self.ip);
        let mask = u32::from(self.netmask);
        let network = ip & mask;
        let broadcast = ip | !mask;
        ((network + 1)..broadcast).map(Ipv4Addr::from)
    }
}

/// All usable IPv4 addresses on this machine, loopback excluded.
///
/// Shown on the receiver screen so the user can read their address off the
/// laptop if discovery is blocked, and used to work out what to scan.
pub fn local_addresses() -> Vec<LocalAddress> {
    let Ok(interfaces) = if_addrs::get_if_addrs() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for interface in interfaces {
        if interface.is_loopback() {
            continue;
        }
        if let if_addrs::IfAddr::V4(v4) = interface.addr {
            if v4.ip.is_link_local() {
                continue;
            }
            out.push(LocalAddress {
                interface: interface.name,
                ip: v4.ip,
                netmask: v4.netmask,
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn broadcast_and_hosts() {
        let addr = LocalAddress {
            interface: "wifi".into(),
            ip: Ipv4Addr::new(192, 168, 1, 37),
            netmask: Ipv4Addr::new(255, 255, 255, 0),
        };
        assert_eq!(addr.broadcast(), Ipv4Addr::new(192, 168, 1, 255));
        assert_eq!(addr.subnet_size(), 256);
        let hosts: Vec<_> = addr.hosts().collect();
        assert_eq!(hosts.len(), 254);
        assert_eq!(hosts[0], Ipv4Addr::new(192, 168, 1, 1));
        assert_eq!(hosts[253], Ipv4Addr::new(192, 168, 1, 254));
    }
}
