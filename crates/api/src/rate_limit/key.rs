use std::net::IpAddr;

/// The client as the per-address limits see it, set on the request by the per-IP layer. `None`
/// collects requests whose address is unknown into one shared bucket.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct ClientKey(pub Option<IpAddr>);

impl ClientKey {
    /// IPv6 by its /56, as the live quotas count: a subscriber is usually delegated a /56 and could
    /// take a fresh /64 for every request. IPv4-mapped IPv6 counts as the IPv4 address it carries.
    pub(crate) fn of(ip: Option<IpAddr>) -> Self {
        Self(office_live::quota_network(ip))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(s: &str) -> ClientKey {
        ClientKey::of(Some(s.parse().expect("ip")))
    }

    #[test]
    fn ipv6_addresses_of_one_56_share_a_key_as_in_the_live_quotas() {
        assert_eq!(
            key("2001:db8:1:200::1"),
            key("2001:db8:1:2ff:ffff:ffff:ffff:ffff")
        );
        assert_ne!(key("2001:db8:1:200::1"), key("2001:db8:1:300::1"));
        for ip in ["2001:db8:1:2ab::9", "::ffff:203.0.113.1", "203.0.113.1"] {
            let ip: IpAddr = ip.parse().expect("ip");
            assert_eq!(
                ClientKey::of(Some(ip)).0,
                office_live::quota_network(Some(ip))
            );
        }
    }

    #[test]
    fn ipv4_is_kept_whole_and_mapped_ipv6_counts_as_ipv4() {
        assert_ne!(key("203.0.113.1"), key("203.0.113.2"));
        assert_eq!(key("::ffff:203.0.113.1"), key("203.0.113.1"));
        assert_eq!(ClientKey::of(None), ClientKey(None));
    }
}
