use std::{fmt, net::IpAddr, str::FromStr};

/// An address range such as `127.0.0.0/8` or `::1`; a bare address is a single host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IpNet {
    network: IpAddr,
    prefix: u8,
}

impl IpNet {
    pub fn contains(&self, ip: IpAddr) -> bool {
        match (self.network, ip.to_canonical()) {
            (IpAddr::V4(net), IpAddr::V4(ip)) => {
                prefix_matches(u32::from(net).into(), u32::from(ip).into(), self.prefix, 32)
            }
            (IpAddr::V6(net), IpAddr::V6(ip)) => {
                prefix_matches(u128::from(net), u128::from(ip), self.prefix, 128)
            }
            _ => false,
        }
    }
}

fn prefix_matches(net: u128, ip: u128, prefix: u8, bits: u8) -> bool {
    let host_bits = u32::from(bits - prefix);
    // An IPv6 /0 shifts by all 128 bits, which `checked_shr` refuses: both sides become 0, a match.
    net.checked_shr(host_bits).unwrap_or(0) == ip.checked_shr(host_bits).unwrap_or(0)
}

#[derive(Debug, PartialEq, Eq)]
pub struct InvalidIpNet;

impl fmt::Display for InvalidIpNet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("not an IP address or CIDR range")
    }
}

impl std::error::Error for InvalidIpNet {}

impl FromStr for IpNet {
    type Err = InvalidIpNet;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (addr, prefix) = match s.trim().split_once('/') {
            Some((addr, prefix)) => (addr, Some(prefix)),
            None => (s.trim(), None),
        };
        let network = addr
            .parse::<IpAddr>()
            .map_err(|_| InvalidIpNet)?
            .to_canonical();
        let bits = if network.is_ipv4() { 32 } else { 128 };
        let prefix = match prefix {
            None => bits,
            Some(p) => p
                .parse::<u8>()
                .ok()
                .filter(|p| *p <= bits)
                .ok_or(InvalidIpNet)?,
        };
        Ok(Self { network, prefix })
    }
}

/// Peers whose `X-Forwarded-For` is believed: the reverse proxy, as seen from the server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrustedProxies(Vec<IpNet>);

impl TrustedProxies {
    /// Loopback only: the server runs behind a proxy on the same host.
    pub fn loopback() -> Self {
        Self(vec![
            IpNet {
                network: IpAddr::from([127, 0, 0, 0]),
                prefix: 8,
            },
            IpNet {
                network: IpAddr::from([0u16, 0, 0, 0, 0, 0, 0, 1]),
                prefix: 128,
            },
        ])
    }

    pub fn contains(&self, ip: IpAddr) -> bool {
        self.0.iter().any(|net| net.contains(ip))
    }
}

impl Default for TrustedProxies {
    fn default() -> Self {
        Self::loopback()
    }
}

/// A comma-separated list; at least one entry.
impl FromStr for TrustedProxies {
    type Err = InvalidIpNet;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let nets = s
            .split(',')
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .map(str::parse)
            .collect::<Result<Vec<IpNet>, _>>()?;
        if nets.is_empty() {
            return Err(InvalidIpNet);
        }
        Ok(Self(nets))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().expect("ip")
    }

    #[test]
    fn ranges_match_by_prefix() {
        let net: IpNet = "172.16.0.0/12".parse().expect("net");
        assert!(net.contains(ip("172.16.0.1")));
        assert!(net.contains(ip("172.31.255.254")));
        assert!(!net.contains(ip("172.32.0.1")));
        assert!(!net.contains(ip("::1")));
        let v6: IpNet = "fd00::/8".parse().expect("net");
        assert!(v6.contains(ip("fd12::5")));
        assert!(!v6.contains(ip("fe80::1")));
        let all: IpNet = "0.0.0.0/0".parse().expect("net");
        assert!(all.contains(ip("203.0.113.9")));
    }

    #[test]
    fn a_bare_address_is_one_host_and_mapped_v4_counts_as_v4() {
        let host: IpNet = "172.18.0.1".parse().expect("net");
        assert!(host.contains(ip("172.18.0.1")));
        assert!(!host.contains(ip("172.18.0.2")));
        assert!(host.contains(ip("::ffff:172.18.0.1")));
    }

    #[test]
    fn lists_parse_and_bad_entries_are_rejected() {
        let list: TrustedProxies = " 127.0.0.1 , 10.0.0.0/8,".parse().expect("list");
        assert!(list.contains(ip("10.1.2.3")));
        assert!(!list.contains(ip("192.168.1.1")));
        for bad in [
            "",
            " , ",
            "10.0.0.0/33",
            "::/129",
            "localhost",
            "10.0.0.0/x",
        ] {
            assert_eq!(bad.parse::<TrustedProxies>(), Err(InvalidIpNet), "{bad:?}");
        }
    }

    #[test]
    fn default_is_loopback() {
        let list = TrustedProxies::default();
        assert!(list.contains(ip("127.0.0.1")));
        assert!(list.contains(ip("::1")));
        assert!(!list.contains(ip("172.17.0.1")));
    }
}
