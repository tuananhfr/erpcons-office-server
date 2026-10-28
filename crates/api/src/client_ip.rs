use std::net::{IpAddr, SocketAddr};

use axum::{
    extract::{ConnectInfo, Request},
    http::HeaderMap,
};

use crate::ApiConfig;

/// The caller's address: the last `X-Forwarded-For` entry when the peer is a trusted proxy, else
/// the peer. A peer that is not the proxy could write any header it likes.
pub(crate) fn client_ip(request: &Request, config: &ApiConfig) -> Option<IpAddr> {
    let peer = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|info| info.0.ip());
    if config.trust_forwarded_for
        && let Some(peer) = peer
        && config.trusted_proxies.contains(peer)
        && let Some(ip) = last_forwarded_for(request.headers())
    {
        return Some(ip);
    }
    peer
}

/// Entries left of the last one were written by the client and prove nothing.
fn last_forwarded_for(headers: &HeaderMap) -> Option<IpAddr> {
    let value = headers.get_all("x-forwarded-for").iter().next_back()?;
    value.to_str().ok()?.rsplit(',').next()?.trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use axum::body::Body;

    use super::*;

    fn request_from(peer: &str, forwarded: &[&str]) -> Request {
        let mut builder = Request::builder();
        for value in forwarded {
            builder = builder.header("x-forwarded-for", *value);
        }
        let mut request = builder.body(Body::empty()).expect("request");
        let peer: SocketAddr = peer.parse().expect("addr");
        request.extensions_mut().insert(ConnectInfo(peer));
        request
    }

    fn request(forwarded: &[&str]) -> Request {
        request_from("127.0.0.1:5000", forwarded)
    }

    fn trusting(proxies: &str) -> ApiConfig {
        ApiConfig {
            trust_forwarded_for: true,
            trusted_proxies: proxies.parse().expect("proxies"),
            ..ApiConfig::default()
        }
    }

    fn ip(s: &str) -> Option<IpAddr> {
        Some(s.parse().expect("ip"))
    }

    #[test]
    fn trusted_proxy_uses_the_last_entry() {
        let config = trusting("127.0.0.1");
        assert_eq!(
            client_ip(&request(&["203.0.113.9"]), &config),
            ip("203.0.113.9")
        );
        let spoofed = request(&["198.51.100.1, 203.0.113.9"]);
        assert_eq!(client_ip(&spoofed, &config), ip("203.0.113.9"));
        let two_headers = request(&["198.51.100.1", "2001:db8::1"]);
        assert_eq!(client_ip(&two_headers, &config), ip("2001:db8::1"));
    }

    #[test]
    fn a_docker_gateway_peer_is_trusted_when_listed() {
        let config = trusting("172.16.0.0/12");
        let via_gateway = request_from("172.18.0.1:40000", &["203.0.113.9"]);
        assert_eq!(client_ip(&via_gateway, &config), ip("203.0.113.9"));
    }

    #[test]
    fn a_peer_outside_the_trusted_proxies_keeps_its_own_address() {
        let config = trusting("127.0.0.1");
        let direct = request_from("198.51.100.7:40000", &["203.0.113.9"]);
        assert_eq!(client_ip(&direct, &config), ip("198.51.100.7"));
        let config = trusting("10.0.0.0/8");
        assert_eq!(
            client_ip(&request(&["203.0.113.9"]), &config),
            ip("127.0.0.1")
        );
    }

    #[test]
    fn untrusted_or_unusable_header_falls_back_to_the_peer() {
        let off = ApiConfig {
            trust_forwarded_for: false,
            ..trusting("127.0.0.1")
        };
        assert_eq!(client_ip(&request(&["203.0.113.9"]), &off), ip("127.0.0.1"));
        let config = trusting("127.0.0.1");
        assert_eq!(
            client_ip(&request(&["not-an-ip"]), &config),
            ip("127.0.0.1")
        );
        assert_eq!(client_ip(&request(&[]), &config), ip("127.0.0.1"));
    }
}
