use office_documents::{LinkKey, QuotaConfig};
use office_mail::MailPolicy;

use crate::TrustedProxies;

/// Token bucket: up to `burst` requests at once, refilled at `per_second`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RateConfig {
    pub burst: u32,
    pub per_second: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ApiConfig {
    pub quota: QuotaConfig,
    /// Largest request body accepted, by the upload and save routes; JSON bodies stop at
    /// [`JSON_BODY_BYTES`].
    pub max_body_bytes: u64,
    pub ip_rate: RateConfig,
    pub actor_rate: RateConfig,
    /// Per client address, the token checks that need a signature check (not cached, well-formed).
    pub verify_rate: RateConfig,
    /// Per client address, opening public view links (no sign-in, so no per-actor limit).
    pub link_rate: RateConfig,
    /// Whether sharing queues notification mail.
    pub mail: MailPolicy,
    /// `ERPCONS_LINK_KEY`; the server refuses to start without it. `None` only in tests that
    /// never turn a link on.
    pub link_key: Option<LinkKey>,
    /// Uploads and saves one actor may stream at once.
    pub max_concurrent_uploads: usize,
    /// Take the client address from the last `X-Forwarded-For` entry, which the reverse proxy
    /// writes, but only on connections from `trusted_proxies`.
    pub trust_forwarded_for: bool,
    pub trusted_proxies: TrustedProxies,
}

/// Room above the per-file limit for multipart framing and the other form fields.
pub const BODY_OVERHEAD_BYTES: u64 = 1024 * 1024;

/// JSON bodies are a few names and ids; anything near this is not a real request.
pub const JSON_BODY_BYTES: u64 = 64 * 1024;

impl Default for ApiConfig {
    fn default() -> Self {
        let quota = QuotaConfig::default();
        Self {
            quota,
            max_body_bytes: quota.max_file_bytes + BODY_OVERHEAD_BYTES,
            // Generous per address: offices share one public IP behind NAT.
            ip_rate: RateConfig {
                burst: 300,
                per_second: 30.0,
            },
            actor_rate: RateConfig {
                burst: 120,
                per_second: 10.0,
            },
            // An office behind one address checks each token about once a minute (cache TTL).
            verify_rate: RateConfig {
                burst: 60,
                per_second: 5.0,
            },
            // A view is two requests (metadata, bytes); a class opening one link shares an address.
            link_rate: RateConfig {
                burst: 60,
                per_second: 2.0,
            },
            mail: MailPolicy::default(),
            link_key: None,
            max_concurrent_uploads: 3,
            trust_forwarded_for: false,
            trusted_proxies: TrustedProxies::loopback(),
        }
    }
}
