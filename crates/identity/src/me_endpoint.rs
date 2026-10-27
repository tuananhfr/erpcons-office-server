use std::time::{Duration, Instant};

use async_trait::async_trait;
use reqwest::{StatusCode, header::ACCEPT, redirect};
use serde::Deserialize;
use time::OffsetDateTime;
use tracing::warn;

use crate::{
    Actor, IdentityVerifier, VerifyError,
    cache::{VerdictCache, hash_token},
};

const MAX_TOKEN_LEN: usize = 4096;
const MAX_ACTOR_ID_LEN: usize = 255;

#[derive(Clone, Debug)]
pub struct MeEndpointConfig {
    /// PI Platform API base, without `/v1`.
    pub base_url: String,
    pub timeout: Duration,
    pub positive_ttl: Duration,
    /// Short, so a token refreshed right after a rejection is not refused for long.
    pub negative_ttl: Duration,
    pub max_cache_entries: usize,
}

impl MeEndpointConfig {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            timeout: Duration::from_secs(5),
            positive_ttl: Duration::from_secs(60),
            negative_ttl: Duration::from_secs(5),
            max_cache_entries: 100_000,
        }
    }
}

/// Verifies a token by calling PI `GET /v1/me` with it (token passthrough, plan §2.1): the
/// stopgap until PI offers introspection or JWT access tokens for Office.
pub struct MeEndpointVerifier {
    http: reqwest::Client,
    me_url: String,
    cache: VerdictCache,
    positive_ttl: Duration,
    negative_ttl: Duration,
}

#[derive(Deserialize)]
struct MeBody {
    user_id: String,
    display_name: String,
    #[serde(default)]
    status: Option<String>,
}

impl MeEndpointVerifier {
    pub fn new(config: MeEndpointConfig) -> Result<Self, reqwest::Error> {
        // reqwest is built without a crypto provider (workspace Cargo.toml); this installs ring once
        // per process and is a no-op when a provider is already installed.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let http = reqwest::Client::builder()
            .timeout(config.timeout)
            .connect_timeout(config.timeout.min(Duration::from_secs(2)))
            // A redirect must never carry the user's token anywhere else.
            .redirect(redirect::Policy::none())
            .user_agent(concat!("erpcons-office-server/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Self {
            http,
            me_url: format!("{}/v1/me", config.base_url.trim_end_matches('/')),
            cache: VerdictCache::new(config.max_cache_entries),
            positive_ttl: config.positive_ttl,
            negative_ttl: config.negative_ttl,
        })
    }

    async fn ask_pi(&self, token: &str) -> Result<Actor, VerifyError> {
        let response = self
            .http
            .get(&self.me_url)
            .bearer_auth(token)
            .header(ACCEPT, "application/json")
            .send()
            .await
            .map_err(|e| {
                warn!(
                    timeout = e.is_timeout(),
                    connect = e.is_connect(),
                    "PI /v1/me unreachable"
                );
                VerifyError::Unavailable
            })?;
        match response.status() {
            StatusCode::OK => {}
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
                return Err(VerifyError::Unauthenticated);
            }
            status => {
                warn!(status = status.as_u16(), "PI /v1/me failed");
                return Err(VerifyError::Unavailable);
            }
        }
        let body: MeBody = response.json().await.map_err(|_| {
            warn!("PI /v1/me returned an unreadable body");
            VerifyError::Unavailable
        })?;
        // Allowlist: a missing or unknown status breaks the contract and must never let a caller in.
        match body.status.as_deref() {
            Some("active") => {}
            Some("suspended") => return Err(VerifyError::Unauthenticated),
            _ => {
                warn!("PI /v1/me returned no usable account status");
                return Err(VerifyError::Unavailable);
            }
        }
        if body.user_id.is_empty() || body.user_id.len() > MAX_ACTOR_ID_LEN {
            warn!("PI /v1/me returned an unusable user_id");
            return Err(VerifyError::Unavailable);
        }
        Ok(Actor {
            actor_id: body.user_id,
            display_name: body.display_name,
            expires_at: OffsetDateTime::now_utc() + self.positive_ttl,
        })
    }
}

/// Visible ASCII only, so the token is a valid header value and can be sent to PI as is.
fn plausible(token: &str) -> bool {
    !token.is_empty() && token.len() <= MAX_TOKEN_LEN && token.bytes().all(|b| b.is_ascii_graphic())
}

#[async_trait]
impl IdentityVerifier for MeEndpointVerifier {
    async fn verify(&self, token: &str) -> Result<Actor, VerifyError> {
        if !plausible(token) {
            return Err(VerifyError::Unauthenticated);
        }
        let key = hash_token(token);
        if let Some(verdict) = self.cache.get(&key, Instant::now()) {
            return verdict;
        }
        let verdict = self.ask_pi(token).await;
        let ttl = match verdict {
            Ok(_) => self.positive_ttl,
            Err(VerifyError::Unauthenticated) => self.negative_ttl,
            Err(VerifyError::Unavailable) => Duration::ZERO,
        };
        self.cache.insert(key, verdict.clone(), ttl, Instant::now());
        verdict
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plausible_tokens_are_visible_ascii() {
        assert!(plausible("abc.DEF-123_~+/="));
        assert!(!plausible(""));
        assert!(!plausible("a b"));
        assert!(!plausible("a\r\nX-Injected: 1"));
        assert!(!plausible("tökén"));
        assert!(!plausible(&"a".repeat(MAX_TOKEN_LEN + 1)));
    }
}
