use std::net::{IpAddr, Ipv6Addr};

/// The client network the live channel's per-address quotas count: an IPv4 address whole, an IPv6
/// address by its /56. A subscriber is usually delegated a /56 (or a /48), so counting by /64 would
/// give one household 256 quotas. IPv4-mapped IPv6 counts as the IPv4 address it carries.
pub fn quota_network(ip: Option<IpAddr>) -> Option<IpAddr> {
    ip.map(|ip| match ip {
        IpAddr::V4(_) => ip,
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => IpAddr::V4(v4),
            None => IpAddr::V6(Ipv6Addr::from(u128::from(v6) & !((1u128 << 72) - 1))),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn net(s: &str) -> Option<IpAddr> {
        quota_network(Some(s.parse().expect("ip")))
    }

    #[test]
    fn ipv6_counts_by_its_56() {
        assert_eq!(net("2001:db8:1:200::1"), net("2001:db8:1:2ff:ffff::1"));
        assert_eq!(net("2001:db8:1:2ab::"), net("2001:db8:1:200::"));
        assert_ne!(net("2001:db8:1:200::1"), net("2001:db8:1:300::1"));
        assert_eq!(
            net("2001:db8:1:2ab::9"),
            Some("2001:db8:1:200::".parse().expect("ip"))
        );
    }

    #[test]
    fn ipv4_is_whole_and_mapped_counts_as_ipv4() {
        assert_ne!(net("203.0.113.1"), net("203.0.113.2"));
        assert_eq!(net("::ffff:203.0.113.1"), net("203.0.113.1"));
        assert_eq!(quota_network(None), None);
    }
}
