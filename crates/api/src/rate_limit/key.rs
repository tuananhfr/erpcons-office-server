use std::net::{IpAddr, Ipv6Addr};

/// The client as the per-address limits see it, set on the request by the per-IP layer. `None`
/// collects requests whose address is unknown into one shared bucket.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct ClientKey(pub Option<IpAddr>);

impl ClientKey {
    pub(crate) fn of(ip: Option<IpAddr>) -> Self {
        Self(ip.map(network_of))
    }
}

/// IPv6 by its /64: one subscriber usually holds a whole /64 and could take a fresh address for
/// every request. IPv4-mapped IPv6 counts as the IPv4 address it carries.
fn network_of(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V4(_) => ip,
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => IpAddr::V4(v4),
            None => {
                let prefix = u128::from(v6) & !((1u128 << 64) - 1);
                IpAddr::V6(Ipv6Addr::from(prefix))
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(s: &str) -> ClientKey {
        ClientKey::of(Some(s.parse().expect("ip")))
    }

    #[test]
    fn ipv6_addresses_of_one_64_share_a_key() {
        assert_eq!(
            key("2001:db8:1:2::1"),
            key("2001:db8:1:2:ffff:ffff:ffff:ffff")
        );
        assert_ne!(key("2001:db8:1:2::1"), key("2001:db8:1:3::1"));
        assert_eq!(key("2001:db8:1:2::1"), key("2001:db8:1:2::"));
    }

    #[test]
    fn ipv4_is_kept_whole_and_mapped_ipv6_counts_as_ipv4() {
        assert_ne!(key("203.0.113.1"), key("203.0.113.2"));
        assert_eq!(key("::ffff:203.0.113.1"), key("203.0.113.1"));
        assert_eq!(ClientKey::of(None), ClientKey(None));
    }
}
