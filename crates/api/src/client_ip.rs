use std::net::{IpAddr, SocketAddr};

use axum::{
    extract::{ConnectInfo, Request},
    http::HeaderMap,
};

/// The caller's address: the last `X-Forwarded-For` entry when the proxy is trusted, else the peer.
pub(crate) fn client_ip(request: &Request, trust_forwarded_for: bool) -> Option<IpAddr> {
    if trust_forwarded_for && let Some(ip) = last_forwarded_for(request.headers()) {
        return Some(ip);
    }
    request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|info| info.0.ip())
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

    fn request(forwarded: &[&str]) -> Request {
        let mut builder = Request::builder();
        for value in forwarded {
            builder = builder.header("x-forwarded-for", *value);
        }
        let mut request = builder.body(Body::empty()).expect("request");
        let peer: SocketAddr = "127.0.0.1:5000".parse().expect("addr");
        request.extensions_mut().insert(ConnectInfo(peer));
        request
    }

    fn ip(s: &str) -> Option<IpAddr> {
        Some(s.parse().expect("ip"))
    }

    #[test]
    fn trusted_proxy_uses_the_last_entry() {
        assert_eq!(
            client_ip(&request(&["203.0.113.9"]), true),
            ip("203.0.113.9")
        );
        let spoofed = request(&["198.51.100.1, 203.0.113.9"]);
        assert_eq!(client_ip(&spoofed, true), ip("203.0.113.9"));
        let two_headers = request(&["198.51.100.1", "2001:db8::1"]);
        assert_eq!(client_ip(&two_headers, true), ip("2001:db8::1"));
    }

    #[test]
    fn untrusted_or_unusable_header_falls_back_to_the_peer() {
        assert_eq!(
            client_ip(&request(&["203.0.113.9"]), false),
            ip("127.0.0.1")
        );
        assert_eq!(client_ip(&request(&["not-an-ip"]), true), ip("127.0.0.1"));
        assert_eq!(client_ip(&request(&[]), true), ip("127.0.0.1"));
    }
}
