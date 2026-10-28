use std::{
    collections::HashMap,
    sync::{Arc, Mutex, MutexGuard, PoisonError},
    time::{Duration, Instant},
};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use reqwest::{StatusCode, header::ACCEPT};
use serde::Deserialize;
use tracing::warn;

use crate::{VerifyError, http, jwt::valid_kid, single_flight::SingleFlight};

/// A raw Ed25519 public key (RFC 8037 `x`).
pub(crate) type PublicKey = [u8; 32];

const MAX_JWKS_BYTES: usize = 64 * 1024;
const MAX_KEYS: usize = 32;

/// PI's signing keys by `kid`. Keys once fetched stay usable while PI is unreachable; a stale list
/// is refreshed in the background, an unknown `kid` refreshes it at most once per `min_refresh`.
pub(crate) struct Jwks {
    inner: Arc<Inner>,
}

struct Inner {
    http: reqwest::Client,
    url: String,
    timeout: Duration,
    ttl: Duration,
    min_refresh: Duration,
    state: Mutex<State>,
    flight: SingleFlight<(), ()>,
}

#[derive(Default)]
struct State {
    keys: HashMap<String, PublicKey>,
    fetched_at: Option<Instant>,
    last_attempt: Option<Instant>,
    last_failed: bool,
}

pub(crate) struct JwksConfig {
    pub(crate) url: String,
    pub(crate) timeout: Duration,
    pub(crate) ttl: Duration,
    pub(crate) min_refresh: Duration,
}

impl Jwks {
    pub(crate) fn new(http: reqwest::Client, config: JwksConfig) -> Self {
        Self {
            inner: Arc::new(Inner {
                http,
                url: config.url,
                timeout: config.timeout,
                ttl: config.ttl,
                min_refresh: config.min_refresh,
                state: Mutex::new(State::default()),
                flight: SingleFlight::new(),
            }),
        }
    }

    /// The key PI published under `kid`.
    pub(crate) async fn key(&self, kid: &str) -> Result<PublicKey, VerifyError> {
        let inner = &self.inner;
        let (known, refresh_soon) = {
            let state = inner.lock();
            let now = Instant::now();
            let stale = state
                .fetched_at
                .is_none_or(|at| now.duration_since(at) >= inner.ttl);
            (
                state.keys.get(kid).copied(),
                stale && state.due(now, inner.min_refresh),
            )
        };
        if let Some(key) = known {
            if refresh_soon {
                self.refresh_in_background();
            }
            return Ok(key);
        }
        inner.flight.run((), || inner.refresh_if_due()).await;
        let state = inner.lock();
        match state.keys.get(kid) {
            Some(key) => Ok(*key),
            // PI answered with a list that lacks this kid: the token is not PI's.
            None if state.fetched_at.is_some() && !state.last_failed => {
                Err(VerifyError::Unauthenticated)
            }
            None => Err(VerifyError::Unavailable),
        }
    }

    fn refresh_in_background(&self) {
        let inner = Arc::clone(&self.inner);
        tokio::spawn(async move {
            inner.flight.run((), || inner.refresh_if_due()).await;
        });
    }
}

impl State {
    fn due(&self, now: Instant, min_refresh: Duration) -> bool {
        self.last_attempt
            .is_none_or(|at| now.duration_since(at) >= min_refresh)
    }
}

impl Inner {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    async fn refresh_if_due(&self) {
        {
            let mut state = self.lock();
            let now = Instant::now();
            if !state.due(now, self.min_refresh) {
                return;
            }
            state.last_attempt = Some(now);
        }
        let fetched = self.fetch().await;
        let mut state = self.lock();
        match fetched {
            Some(keys) => {
                state.keys = keys;
                state.fetched_at = Some(Instant::now());
                state.last_failed = false;
            }
            // The previous keys stay: an outage of PI must not sign everybody out.
            None => state.last_failed = true,
        }
    }

    async fn fetch(&self) -> Option<HashMap<String, PublicKey>> {
        let response = self
            .http
            .get(&self.url)
            .header(ACCEPT, "application/json")
            .timeout(self.timeout)
            .send()
            .await
            .map_err(|e| {
                warn!(
                    timeout = e.is_timeout(),
                    connect = e.is_connect(),
                    "PI JWKS unreachable"
                );
            })
            .ok()?;
        if response.status() != StatusCode::OK {
            warn!(status = response.status().as_u16(), "PI JWKS fetch failed");
            return None;
        }
        let Some(body) = http::read_capped(response, MAX_JWKS_BYTES).await else {
            warn!("PI JWKS body unreadable or too large");
            return None;
        };
        let keys = parse(&body);
        if keys.is_none() {
            warn!("PI JWKS holds no usable Ed25519 key");
        }
        keys
    }
}

#[derive(Deserialize)]
struct JwkSet {
    keys: Vec<serde_json::Value>,
}

#[derive(Deserialize)]
struct Jwk {
    kty: String,
    crv: String,
    x: String,
    kid: String,
    alg: Option<String>,
    #[serde(rename = "use")]
    usage: Option<String>,
}

impl Jwk {
    fn ed25519(self) -> Option<(String, PublicKey)> {
        let usable = self.kty == "OKP"
            && self.crv == "Ed25519"
            && self.alg.as_deref().is_none_or(|a| a == "EdDSA")
            && self.usage.as_deref().is_none_or(|u| u == "sig")
            && valid_kid(&self.kid);
        if !usable {
            return None;
        }
        let x = URL_SAFE_NO_PAD.decode(&self.x).ok()?.try_into().ok()?;
        Some((self.kid, x))
    }
}

/// The Ed25519 signing keys of a JWKS (RFC 7517); keys of other kinds are skipped, an empty
/// result is `None`.
fn parse(body: &[u8]) -> Option<HashMap<String, PublicKey>> {
    let set: JwkSet = serde_json::from_slice(body).ok()?;
    let mut keys = HashMap::new();
    for (kid, key) in set
        .keys
        .into_iter()
        .filter_map(|k| serde_json::from_value::<Jwk>(k).ok()?.ed25519())
        .take(MAX_KEYS)
    {
        keys.entry(kid).or_insert(key);
    }
    (!keys.is_empty()).then_some(keys)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn x(byte: u8) -> String {
        URL_SAFE_NO_PAD.encode([byte; 32])
    }

    #[test]
    fn pi_jwks_parses() {
        let body = json!({ "keys": [
            { "kty": "OKP", "crv": "Ed25519", "x": x(1), "kid": "2026-10", "use": "sig", "alg": "EdDSA" },
            { "kty": "OKP", "crv": "Ed25519", "x": x(2), "kid": "2026-04" },
        ]});
        let keys = parse(body.to_string().as_bytes()).expect("keys");
        assert_eq!(keys.len(), 2);
        assert_eq!(keys["2026-10"], [1; 32]);
        assert_eq!(keys["2026-04"], [2; 32]);
    }

    #[test]
    fn keys_of_other_kinds_are_skipped() {
        let body = json!({ "keys": [
            { "kty": "RSA", "n": "AQAB", "e": "AQAB", "kid": "rsa" },
            { "kty": "OKP", "crv": "X25519", "x": x(3), "kid": "ecdh" },
            { "kty": "OKP", "crv": "Ed25519", "x": x(4), "kid": "enc", "use": "enc" },
            { "kty": "OKP", "crv": "Ed25519", "x": x(5), "kid": "hs", "alg": "HS256" },
            { "kty": "OKP", "crv": "Ed25519", "x": URL_SAFE_NO_PAD.encode([6; 31]), "kid": "short" },
            { "kty": "OKP", "crv": "Ed25519", "x": x(7), "kid": "bad/kid" },
            { "kty": "OKP", "crv": "Ed25519", "x": x(8), "kid": "dup" },
            { "kty": "OKP", "crv": "Ed25519", "x": x(9), "kid": "dup" },
            "not an object",
        ]});
        let keys = parse(body.to_string().as_bytes()).expect("keys");
        assert_eq!(keys.len(), 1, "{:?}", keys.keys().collect::<Vec<_>>());
        assert_eq!(keys["dup"], [8; 32], "the first key of a kid wins");
    }

    #[test]
    fn nothing_usable_is_none() {
        assert_eq!(parse(br#"{"keys":[]}"#), None);
        assert_eq!(parse(br#"{"keys":[{"kty":"RSA"}]}"#), None);
        assert_eq!(parse(b"[]"), None);
        assert_eq!(parse(b"<html>"), None);
    }
}
