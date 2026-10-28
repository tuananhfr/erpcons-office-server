use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use time::OffsetDateTime;
use tracing::debug;

use crate::{
    Actor, IdentityVerifier, VerifyError,
    cache::{TokenHash, VerdictCache, hash_token},
    display_name::{DisplayNames, DisplayNamesConfig},
    http,
    jwks::{Jwks, JwksConfig},
    jwt::{self, MAX_TOKEN_LEN, Policy, Reject},
    single_flight::SingleFlight,
};

#[derive(Clone, Debug)]
pub struct JwtVerifierConfig {
    /// PI's issuer, exactly as its tokens' `iss`; the JWKS (`/id/jwks.json`) and userinfo
    /// (`/id/userinfo`) URLs are derived from it, as in PI's discovery document.
    pub issuer: String,
    /// A token is accepted when its `aud` names one of these.
    pub audiences: Vec<String>,
    /// OAuth clients (`azp`) whose tokens Office accepts.
    pub allowed_clients: Vec<String>,
    /// Total time of a JWKS fetch.
    pub timeout: Duration,
    /// Total time of a userinfo call; short, since a request waits for it.
    pub name_timeout: Duration,
    /// Age after which the keys are refreshed in the background (PI: `max-age=300`).
    pub jwks_ttl: Duration,
    /// At most one JWKS fetch per this interval, however many unknown `kid`s arrive.
    pub jwks_min_refresh: Duration,
    /// Clock skew allowed on `exp`, `nbf` and `iat` (PI's own check allows 30 s).
    pub leeway: Duration,
    /// A verdict is reused this long, never past the token's `exp`.
    pub positive_ttl: Duration,
    pub negative_ttl: Duration,
    /// A display name is asked of PI again after this long.
    pub name_ttl: Duration,
    /// After PI failed to give a name, the next try waits this long.
    pub name_retry: Duration,
    pub max_cache_entries: usize,
}

impl JwtVerifierConfig {
    pub fn new(issuer: impl Into<String>) -> Self {
        Self {
            issuer: issuer.into(),
            audiences: vec!["pi-platform".into()],
            allowed_clients: vec!["erpcons-office-web".into()],
            timeout: Duration::from_secs(5),
            name_timeout: Duration::from_secs(2),
            jwks_ttl: Duration::from_secs(300),
            jwks_min_refresh: Duration::from_secs(10),
            leeway: Duration::from_secs(30),
            positive_ttl: Duration::from_secs(60),
            negative_ttl: Duration::from_secs(5),
            name_ttl: Duration::from_secs(3600),
            name_retry: Duration::from_secs(60),
            max_cache_entries: 100_000,
        }
    }
}

/// Verifies PI ID access tokens locally (EdDSA JWT, keys from PI's JWKS): once the keys are
/// cached, authentication no longer depends on PI being reachable.
pub struct JwtVerifier {
    issuer: String,
    audiences: Vec<String>,
    clients: Vec<String>,
    leeway_secs: i64,
    jwks: Jwks,
    names: DisplayNames,
    cache: VerdictCache,
    /// One check per token at a time: a burst of requests with a fresh token shares it.
    inflight: SingleFlight<TokenHash, Result<Actor, VerifyError>>,
    positive_ttl: Duration,
    negative_ttl: Duration,
}

impl JwtVerifier {
    pub fn new(config: JwtVerifierConfig) -> Result<Self, reqwest::Error> {
        // PI trims the trailing slash of its issuer, so its tokens never carry one.
        let issuer = config.issuer.trim_end_matches('/').to_owned();
        let http = http::client(config.timeout.min(Duration::from_secs(2)))?;
        let jwks = Jwks::new(
            http.clone(),
            JwksConfig {
                url: format!("{issuer}/id/jwks.json"),
                timeout: config.timeout,
                ttl: config.jwks_ttl,
                min_refresh: config.jwks_min_refresh,
            },
        );
        let names = DisplayNames::new(
            http,
            DisplayNamesConfig {
                url: format!("{issuer}/id/userinfo"),
                timeout: config.name_timeout,
                ttl: config.name_ttl,
                retry: config.name_retry,
                max_entries: config.max_cache_entries,
            },
        );
        Ok(Self {
            issuer,
            audiences: config.audiences,
            clients: config.allowed_clients,
            leeway_secs: i64::try_from(config.leeway.as_secs()).unwrap_or(i64::MAX),
            jwks,
            names,
            cache: VerdictCache::new(config.max_cache_entries),
            inflight: SingleFlight::new(),
            positive_ttl: config.positive_ttl,
            negative_ttl: config.negative_ttl,
        })
    }

    async fn resolve(&self, token: &str) -> (Result<Actor, VerifyError>, Duration) {
        let accepted = match self.check(token).await {
            Ok(accepted) => accepted,
            Err(e @ VerifyError::Unauthenticated) => return (Err(e), self.negative_ttl),
            Err(e @ VerifyError::Unavailable) => return (Err(e), Duration::ZERO),
        };
        let remaining = u64::try_from(accepted.exp.saturating_sub(unix_now())).unwrap_or(0);
        let ttl = self.positive_ttl.min(Duration::from_secs(remaining));
        let display_name = self.names.get(&accepted.sub, token).await;
        let actor = Actor {
            actor_id: accepted.sub,
            display_name,
            expires_at: OffsetDateTime::now_utc() + ttl,
        };
        (Ok(actor), ttl)
    }

    async fn check(&self, token: &str) -> Result<jwt::Accepted, VerifyError> {
        let unverified = jwt::split(token).map_err(refuse)?;
        let key = self.jwks.key(&unverified.kid).await?;
        let payload = unverified.verify(&key).map_err(refuse)?;
        let policy = Policy {
            issuer: &self.issuer,
            audiences: &self.audiences,
            clients: &self.clients,
            leeway_secs: self.leeway_secs,
        };
        jwt::check_claims(&payload, &policy, unix_now()).map_err(refuse)
    }
}

fn refuse(reason: Reject) -> VerifyError {
    debug!(reason = reason.as_str(), "access token refused");
    VerifyError::Unauthenticated
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

/// Visible ASCII only, so the token is a valid header value and can be sent to PI as is.
fn plausible(token: &str) -> bool {
    !token.is_empty() && token.len() <= MAX_TOKEN_LEN && token.bytes().all(|b| b.is_ascii_graphic())
}

#[async_trait]
impl IdentityVerifier for JwtVerifier {
    async fn verify(&self, token: &str) -> Result<Actor, VerifyError> {
        if let Some(verdict) = self.cached(token) {
            return verdict;
        }
        let key = hash_token(token);
        self.inflight
            .run(key, || async {
                let (verdict, ttl) = self.resolve(token).await;
                // Cached before the flight ends, so a call arriving after it finds the verdict.
                self.cache.insert(key, verdict.clone(), ttl, Instant::now());
                verdict
            })
            .await
    }

    /// Also answers a token whose form alone refuses it: that needs neither a key nor PI.
    fn cached(&self, token: &str) -> Option<Result<Actor, VerifyError>> {
        if !plausible(token) {
            return Some(Err(VerifyError::Unauthenticated));
        }
        if let Some(verdict) = self.cache.get(&hash_token(token), Instant::now()) {
            return Some(verdict);
        }
        jwt::split(token).err().map(|reason| Err(refuse(reason)))
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

    #[test]
    fn issuer_loses_its_trailing_slash() {
        let verifier =
            JwtVerifier::new(JwtVerifierConfig::new("https://id.pi.example/")).expect("verifier");
        assert_eq!(verifier.issuer, "https://id.pi.example");
    }
}
