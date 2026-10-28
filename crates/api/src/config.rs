use office_documents::QuotaConfig;

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
    /// Largest request body accepted.
    pub max_body_bytes: u64,
    pub ip_rate: RateConfig,
    pub actor_rate: RateConfig,
    /// Take the client address from the last `X-Forwarded-For` entry, which the reverse proxy
    /// writes, but only on connections from `trusted_proxies`.
    pub trust_forwarded_for: bool,
    pub trusted_proxies: TrustedProxies,
}

/// Room above the per-file limit for multipart framing and the other form fields.
pub const BODY_OVERHEAD_BYTES: u64 = 1024 * 1024;

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
            trust_forwarded_for: false,
            trusted_proxies: TrustedProxies::loopback(),
        }
    }
}
