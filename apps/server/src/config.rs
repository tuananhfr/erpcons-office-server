use std::{net::SocketAddr, path::PathBuf, str::FromStr, time::Duration};

use office_api::{ApiConfig, RateConfig, TrustedProxies};
use office_db::PoolConfig;
use office_documents::{
    QuotaConfig,
    jobs::{AuditRetentionConfig, OrphanConfig, PurgeConfig},
};
use office_identity::MeEndpointConfig;

use crate::jobs::JobsConfig;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub(crate) enum ConfigError {
    #[error("{0} is required")]
    Missing(&'static str),
    // The value is never echoed: DATABASE_URL carries the password.
    #[error("{0} has an invalid value")]
    Invalid(&'static str),
}

/// Server settings. No `Debug`, so the database password cannot be logged by accident.
pub(crate) struct ServerConfig {
    pub db: PoolConfig,
    pub data_dir: PathBuf,
    pub bind: SocketAddr,
    pub identity: MeEndpointConfig,
    pub api: ApiConfig,
    pub jobs: JobsConfig,
}

impl ServerConfig {
    pub(crate) fn from_env() -> Result<Self, ConfigError> {
        Self::from_lookup(|name| std::env::var(name).ok())
    }

    fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let vars = Vars(lookup);
        let defaults = ApiConfig::default();

        let mut db = PoolConfig::new(vars.required("DATABASE_URL")?);
        db.max_connections = vars.parsed("ERPCONS_DB_MAX_CONNECTIONS", db.max_connections)?;
        let acquire_secs = vars.parsed(
            "ERPCONS_DB_ACQUIRE_TIMEOUT_SECS",
            db.acquire_timeout.as_secs(),
        )?;
        db.acquire_timeout = Duration::from_secs(acquire_secs);
        if db.max_connections == 0 {
            return Err(ConfigError::Invalid("ERPCONS_DB_MAX_CONNECTIONS"));
        }
        if acquire_secs == 0 {
            return Err(ConfigError::Invalid("ERPCONS_DB_ACQUIRE_TIMEOUT_SECS"));
        }

        let pi_base_url = vars.required("PI_BASE_URL")?;
        if !(pi_base_url.starts_with("https://") || pi_base_url.starts_with("http://")) {
            return Err(ConfigError::Invalid("PI_BASE_URL"));
        }
        let mut identity = MeEndpointConfig::new(pi_base_url);
        let timeout_ms =
            vars.parsed("ERPCONS_PI_TIMEOUT_MS", identity.timeout.as_millis() as u64)?;
        if timeout_ms == 0 {
            return Err(ConfigError::Invalid("ERPCONS_PI_TIMEOUT_MS"));
        }
        identity.timeout = Duration::from_millis(timeout_ms);

        let quota = QuotaConfig {
            limit_bytes: vars.parsed("ERPCONS_QUOTA_BYTES", defaults.quota.limit_bytes)?,
            max_file_bytes: vars.parsed("ERPCONS_MAX_FILE_BYTES", defaults.quota.max_file_bytes)?,
        };
        let default_body = quota
            .max_file_bytes
            .saturating_add(office_api::BODY_OVERHEAD_BYTES);
        let max_body_bytes = vars.parsed("ERPCONS_MAX_BODY_BYTES", default_body)?;
        if max_body_bytes < quota.max_file_bytes {
            return Err(ConfigError::Invalid("ERPCONS_MAX_BODY_BYTES"));
        }
        let api = ApiConfig {
            quota,
            max_body_bytes,
            ip_rate: vars.rate(
                "ERPCONS_RATE_IP_BURST",
                "ERPCONS_RATE_IP_PER_SEC",
                defaults.ip_rate,
            )?,
            actor_rate: vars.rate(
                "ERPCONS_RATE_ACTOR_BURST",
                "ERPCONS_RATE_ACTOR_PER_SEC",
                defaults.actor_rate,
            )?,
            verify_rate: vars.rate(
                "ERPCONS_RATE_VERIFY_BURST",
                "ERPCONS_RATE_VERIFY_PER_SEC",
                defaults.verify_rate,
            )?,
            max_concurrent_uploads: vars.positive(
                "ERPCONS_MAX_CONCURRENT_UPLOADS",
                defaults.max_concurrent_uploads,
            )?,
            // On by default: the server is reachable only through the local Apache proxy (plan §8).
            trust_forwarded_for: vars.parsed("ERPCONS_TRUST_FORWARDED_FOR", true)?,
            trusted_proxies: vars.parsed("ERPCONS_TRUSTED_PROXIES", TrustedProxies::loopback())?,
        };

        let jobs = vars.jobs()?;

        Ok(Self {
            db,
            data_dir: PathBuf::from(vars.required("ERPCONS_DATA_DIR")?),
            bind: vars.parsed("ERPCONS_BIND", SocketAddr::from(([127, 0, 0, 1], 8710)))?,
            identity,
            api,
            jobs,
        })
    }
}

const MIN_ORPHAN_AGE_SECS: u64 = 60;

struct Vars<F>(F);

impl<F: Fn(&str) -> Option<String>> Vars<F> {
    fn get(&self, name: &str) -> Option<String> {
        (self.0)(name).filter(|v| !v.trim().is_empty())
    }

    fn required(&self, name: &'static str) -> Result<String, ConfigError> {
        self.get(name).ok_or(ConfigError::Missing(name))
    }

    fn parsed<T: FromStr>(&self, name: &'static str, default: T) -> Result<T, ConfigError> {
        match self.get(name) {
            None => Ok(default),
            Some(v) => v.trim().parse().map_err(|_| ConfigError::Invalid(name)),
        }
    }

    fn jobs(&self) -> Result<JobsConfig, ConfigError> {
        let every = |name: &'static str| -> Result<Option<Duration>, ConfigError> {
            let secs: u64 = self.parsed(name, 3600)?;
            Ok((secs > 0).then(|| Duration::from_secs(secs)))
        };
        let days: u64 = self.parsed("ERPCONS_TRASH_RETENTION_DAYS", 30)?;
        if days == 0 {
            return Err(ConfigError::Invalid("ERPCONS_TRASH_RETENTION_DAYS"));
        }
        let min_age: u64 = self.parsed("ERPCONS_ORPHAN_MIN_AGE_SECS", 600)?;
        // Younger objects may belong to a save still between its put and its commit.
        if min_age < MIN_ORPHAN_AGE_SECS {
            return Err(ConfigError::Invalid("ERPCONS_ORPHAN_MIN_AGE_SECS"));
        }
        Ok(JobsConfig {
            purge_every: every("ERPCONS_PURGE_INTERVAL_SECS")?,
            purge: PurgeConfig {
                retention: Duration::from_secs(days.saturating_mul(24 * 3600)),
                ..PurgeConfig::default()
            },
            orphans_every: every("ERPCONS_ORPHAN_INTERVAL_SECS")?,
            orphans: OrphanConfig {
                min_age: Duration::from_secs(min_age),
                ..OrphanConfig::default()
            },
            audit: self.audit_retention()?,
        })
    }

    /// `None` when `ERPCONS_AUDIT_RETENTION_DAYS` is 0: audit rows are then kept forever.
    fn audit_retention(&self) -> Result<Option<AuditRetentionConfig>, ConfigError> {
        let defaults = AuditRetentionConfig::default();
        let days: u64 = self.parsed(
            "ERPCONS_AUDIT_RETENTION_DAYS",
            defaults.retention.as_secs() / (24 * 3600),
        )?;
        Ok((days > 0).then(|| AuditRetentionConfig {
            retention: Duration::from_secs(days.saturating_mul(24 * 3600)),
            ..defaults
        }))
    }

    fn positive(&self, name: &'static str, default: usize) -> Result<usize, ConfigError> {
        match self.parsed(name, default)? {
            0 => Err(ConfigError::Invalid(name)),
            n => Ok(n),
        }
    }

    fn rate(
        &self,
        burst: &'static str,
        per_second: &'static str,
        default: RateConfig,
    ) -> Result<RateConfig, ConfigError> {
        let rate = RateConfig {
            burst: self.parsed(burst, default.burst)?,
            per_second: self.parsed(per_second, default.per_second)?,
        };
        if rate.burst == 0 {
            return Err(ConfigError::Invalid(burst));
        }
        if !(rate.per_second.is_finite() && rate.per_second > 0.0) {
            return Err(ConfigError::Invalid(per_second));
        }
        Ok(rate)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    const REQUIRED: [(&str, &str); 3] = [
        ("DATABASE_URL", "postgres://u:secret@db/office"),
        ("ERPCONS_DATA_DIR", "/data"),
        ("PI_BASE_URL", "https://id.pi.example"),
    ];

    fn load(extra: &[(&str, &str)]) -> Result<ServerConfig, ConfigError> {
        let vars: HashMap<String, String> = REQUIRED
            .iter()
            .chain(extra)
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        ServerConfig::from_lookup(|name| vars.get(name).cloned())
    }

    #[test]
    fn defaults_follow_the_plan() {
        let c = load(&[]).expect("config");
        assert_eq!(c.bind, "127.0.0.1:8710".parse().expect("addr"));
        assert_eq!(c.db.max_connections, 10);
        assert_eq!(c.api.quota, QuotaConfig::default());
        assert_eq!(c.api.max_body_bytes, 101 * 1024 * 1024);
        assert!(c.api.trust_forwarded_for);
        assert_eq!(c.api.trusted_proxies, TrustedProxies::loopback());
        assert_eq!(c.identity.base_url, "https://id.pi.example");
        assert_eq!(c.data_dir, PathBuf::from("/data"));
        assert_eq!(c.jobs.purge_every, Some(Duration::from_secs(3600)));
        assert_eq!(c.jobs.purge.retention, Duration::from_secs(30 * 24 * 3600));
        assert_eq!(c.jobs.orphans_every, Some(Duration::from_secs(3600)));
        assert_eq!(c.jobs.orphans.min_age, Duration::from_secs(600));
        assert_eq!(
            c.jobs.audit.map(|a| a.retention),
            Some(Duration::from_secs(180 * 24 * 3600))
        );
        assert_eq!(c.api.max_concurrent_uploads, 3);
        assert_eq!(c.api.verify_rate, ApiConfig::default().verify_rate);
    }

    #[test]
    fn overrides_apply() {
        let c = load(&[
            ("ERPCONS_BIND", "0.0.0.0:8710"),
            ("ERPCONS_DB_MAX_CONNECTIONS", "4"),
            ("ERPCONS_QUOTA_BYTES", "2048"),
            ("ERPCONS_MAX_FILE_BYTES", "1024"),
            ("ERPCONS_RATE_ACTOR_BURST", "5"),
            ("ERPCONS_RATE_ACTOR_PER_SEC", "0.5"),
            ("ERPCONS_TRUST_FORWARDED_FOR", "false"),
            ("ERPCONS_TRUSTED_PROXIES", "172.16.0.0/12, 10.0.0.0/8"),
            ("ERPCONS_PI_TIMEOUT_MS", "1500"),
            ("ERPCONS_PURGE_INTERVAL_SECS", "600"),
            ("ERPCONS_ORPHAN_INTERVAL_SECS", "0"),
            ("ERPCONS_TRASH_RETENTION_DAYS", "7"),
            ("ERPCONS_ORPHAN_MIN_AGE_SECS", "7200"),
            ("ERPCONS_AUDIT_RETENTION_DAYS", "0"),
            ("ERPCONS_MAX_CONCURRENT_UPLOADS", "1"),
            ("ERPCONS_RATE_VERIFY_BURST", "7"),
            ("ERPCONS_RATE_VERIFY_PER_SEC", "0.25"),
        ])
        .expect("config");
        assert_eq!(c.bind.port(), 8710);
        assert!(c.bind.ip().is_unspecified());
        assert_eq!(c.db.max_connections, 4);
        assert_eq!(c.api.quota.limit_bytes, 2048);
        assert_eq!(c.api.max_body_bytes, 1024 + office_api::BODY_OVERHEAD_BYTES);
        assert_eq!(
            c.api.actor_rate,
            RateConfig {
                burst: 5,
                per_second: 0.5
            }
        );
        assert!(!c.api.trust_forwarded_for);
        assert!(
            c.api
                .trusted_proxies
                .contains("172.18.0.1".parse().expect("ip"))
        );
        assert!(
            !c.api
                .trusted_proxies
                .contains("127.0.0.1".parse().expect("ip"))
        );
        assert_eq!(c.identity.timeout, Duration::from_millis(1500));
        assert_eq!(c.jobs.purge_every, Some(Duration::from_secs(600)));
        assert_eq!(c.jobs.orphans_every, None, "0 turns the sweep off");
        assert_eq!(c.jobs.purge.retention, Duration::from_secs(7 * 24 * 3600));
        assert_eq!(c.jobs.orphans.min_age, Duration::from_secs(7200));
        assert_eq!(c.jobs.audit, None, "0 keeps audit rows forever");
        assert_eq!(c.api.max_concurrent_uploads, 1);
        assert_eq!(
            c.api.verify_rate,
            RateConfig {
                burst: 7,
                per_second: 0.25
            }
        );
    }

    #[test]
    fn missing_required_values_are_named() {
        let vars: HashMap<&str, &str> = REQUIRED
            .into_iter()
            .filter(|(k, _)| *k != "PI_BASE_URL")
            .collect();
        let err = ServerConfig::from_lookup(|name| vars.get(name).map(|v| (*v).to_owned()));
        assert_eq!(err.err(), Some(ConfigError::Missing("PI_BASE_URL")));
    }

    #[test]
    fn invalid_values_are_named_without_echoing_them() {
        let err = load(&[("ERPCONS_BIND", "secret-ish")])
            .err()
            .expect("invalid");
        assert_eq!(err, ConfigError::Invalid("ERPCONS_BIND"));
        assert!(!err.to_string().contains("secret-ish"));
        for bad in [
            ("ERPCONS_RATE_IP_PER_SEC", "0"),
            ("ERPCONS_RATE_IP_BURST", "0"),
            ("ERPCONS_MAX_BODY_BYTES", "10"),
            ("PI_BASE_URL", "ftp://pi"),
            ("ERPCONS_TRASH_RETENTION_DAYS", "0"),
            ("ERPCONS_ORPHAN_MIN_AGE_SECS", "59"),
            ("ERPCONS_PURGE_INTERVAL_SECS", "-1"),
            ("ERPCONS_TRUSTED_PROXIES", "10.0.0.0/33"),
            ("ERPCONS_MAX_CONCURRENT_UPLOADS", "0"),
            ("ERPCONS_RATE_VERIFY_PER_SEC", "0"),
            ("ERPCONS_AUDIT_RETENTION_DAYS", "-1"),
        ] {
            assert_eq!(
                load(&[bad]).err(),
                Some(ConfigError::Invalid(bad.0)),
                "{bad:?}"
            );
        }
    }
}
