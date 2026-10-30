use std::{net::SocketAddr, path::PathBuf, str::FromStr, time::Duration};

use office_api::{ApiConfig, RateConfig, TrustedProxies};
use office_db::PoolConfig;
use office_documents::{
    LinkKey, QuotaConfig,
    jobs::{AuditRetentionConfig, OrphanConfig, PurgeConfig},
};
use office_identity::JwtVerifierConfig;
use office_mail::{MailConfig, SmtpConfig, TlsMode};

use crate::jobs::JobsConfig;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub(crate) enum ConfigError {
    #[error("{0} is required")]
    Missing(&'static str),
    // The value is never echoed: DATABASE_URL carries the password.
    #[error("{0} has an invalid value")]
    Invalid(&'static str),
    #[error("ERPCONS_LINK_KEY must be 32 bytes in standard base64 (openssl rand -base64 32)")]
    InvalidLinkKey,
}

/// Server settings. No `Debug`, so the database password cannot be logged by accident.
pub(crate) struct ServerConfig {
    pub db: PoolConfig,
    pub data_dir: PathBuf,
    pub bind: SocketAddr,
    pub identity: JwtVerifierConfig,
    pub api: ApiConfig,
    pub jobs: JobsConfig,
    /// `None` without `ERPCONS_SMTP_HOST`: no mail is queued or sent.
    pub mail: Option<MailConfig>,
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

        let issuer = vars.required("PI_ISSUER")?;
        if !(issuer.starts_with("https://") || issuer.starts_with("http://")) {
            return Err(ConfigError::Invalid("PI_ISSUER"));
        }
        let mut identity = JwtVerifierConfig::new(issuer.trim());
        if let Some(audiences) = vars.list("PI_AUDIENCES")? {
            identity.audiences = audiences;
        }
        if let Some(clients) = vars.list("PI_ALLOWED_CLIENTS")? {
            identity.allowed_clients = clients;
        }
        let timeout_ms =
            vars.parsed("ERPCONS_PI_TIMEOUT_MS", identity.timeout.as_millis() as u64)?;
        if timeout_ms == 0 {
            return Err(ConfigError::Invalid("ERPCONS_PI_TIMEOUT_MS"));
        }
        identity.timeout = Duration::from_millis(timeout_ms);
        let jwks_ttl = vars.parsed("ERPCONS_PI_JWKS_TTL_SECS", identity.jwks_ttl.as_secs())?;
        if jwks_ttl == 0 {
            return Err(ConfigError::Invalid("ERPCONS_PI_JWKS_TTL_SECS"));
        }
        identity.jwks_ttl = Duration::from_secs(jwks_ttl);

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
        let mail = vars.mail()?;
        // Required even before anyone shares a link: a key added later could not open the tokens
        // sealed without it.
        let link_key = LinkKey::from_base64(&vars.required("ERPCONS_LINK_KEY")?)
            .map_err(|_| ConfigError::InvalidLinkKey)?;
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
            link_rate: vars.rate(
                "ERPCONS_RATE_LINK_BURST",
                "ERPCONS_RATE_LINK_PER_SEC",
                defaults.link_rate,
            )?,
            mail: mail.as_ref().map(MailConfig::policy).unwrap_or_default(),
            link_key: Some(link_key),
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
            mail,
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

    /// A comma-separated list; set but empty is invalid.
    fn list(&self, name: &'static str) -> Result<Option<Vec<String>>, ConfigError> {
        let Some(value) = self.get(name) else {
            return Ok(None);
        };
        let items: Vec<String> = value
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect();
        if items.is_empty() {
            return Err(ConfigError::Invalid(name));
        }
        Ok(Some(items))
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

    /// SMTP and what mail needs besides; all of it ignored without `ERPCONS_SMTP_HOST`.
    fn mail(&self) -> Result<Option<MailConfig>, ConfigError> {
        let Some(host) = self.get("ERPCONS_SMTP_HOST") else {
            return Ok(None);
        };
        let tls = match self.get("ERPCONS_SMTP_TLS") {
            None => TlsMode::Starttls,
            Some(v) => TlsMode::parse(&v).ok_or(ConfigError::Invalid("ERPCONS_SMTP_TLS"))?,
        };
        let port: u16 = self.parsed("ERPCONS_SMTP_PORT", tls.default_port())?;
        if port == 0 {
            return Err(ConfigError::Invalid("ERPCONS_SMTP_PORT"));
        }
        let username = self.get("ERPCONS_SMTP_USERNAME");
        let password = self.get("ERPCONS_SMTP_PASSWORD");
        if username.is_some() != password.is_some() {
            return Err(ConfigError::Missing(if username.is_some() {
                "ERPCONS_SMTP_PASSWORD"
            } else {
                "ERPCONS_SMTP_USERNAME"
            }));
        }
        let origin = self.required("ERPCONS_PUBLIC_ORIGIN")?;
        let origin = origin.trim().trim_end_matches('/');
        let has_host = origin
            .split_once("://")
            .is_some_and(|(scheme, rest)| matches!(scheme, "https" | "http") && !rest.is_empty());
        if !has_host || origin.contains(['?', '#', ' ']) {
            return Err(ConfigError::Invalid("ERPCONS_PUBLIC_ORIGIN"));
        }
        Ok(Some(MailConfig {
            smtp: SmtpConfig {
                host: host.trim().to_owned(),
                port,
                tls,
                username,
                password,
            },
            from: self.required("ERPCONS_MAIL_FROM")?.trim().to_owned(),
            public_origin: origin.to_owned(),
            include_title: self.parsed("ERPCONS_MAIL_INCLUDE_TITLE", false)?,
            daily_cap: self.parsed("ERPCONS_MAIL_DAILY_CAP", office_mail::DEFAULT_DAILY_CAP)?,
            global_daily_cap: self.parsed(
                "ERPCONS_MAIL_GLOBAL_DAILY_CAP",
                office_mail::DEFAULT_GLOBAL_DAILY_CAP,
            )?,
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

    /// A development key; never use it outside tests.
    const DEV_LINK_KEY: &str = "ZXJwY29ucy1vZmZpY2UtZGV2LWxpbmsta2V5LTAwMDE=";

    const REQUIRED: [(&str, &str); 4] = [
        ("DATABASE_URL", "postgres://u:secret@db/office"),
        ("ERPCONS_DATA_DIR", "/data"),
        ("PI_ISSUER", "https://id.pi.example"),
        ("ERPCONS_LINK_KEY", DEV_LINK_KEY),
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
        assert_eq!(c.identity.issuer, "https://id.pi.example");
        assert_eq!(c.identity.audiences, ["pi-platform"]);
        assert_eq!(c.identity.allowed_clients, ["erpcons-office-web"]);
        assert_eq!(c.identity.jwks_ttl, Duration::from_secs(300));
        assert_eq!(c.identity.timeout, Duration::from_secs(5));
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
        assert_eq!(c.api.link_rate, ApiConfig::default().link_rate);
        assert!(c.mail.is_none(), "no SMTP host, no mail");
        assert!(!c.api.mail.enabled);
        assert_eq!(
            c.api.link_key,
            Some(LinkKey::from_base64(DEV_LINK_KEY).expect("dev key"))
        );
    }

    const SMTP: [(&str, &str); 3] = [
        ("ERPCONS_SMTP_HOST", "smtp.example.vn"),
        ("ERPCONS_MAIL_FROM", "ERPCons Office <no-reply@erpcons.vn>"),
        ("ERPCONS_PUBLIC_ORIGIN", "https://office.erpcons.vn/"),
    ];

    #[test]
    fn mail_defaults_to_starttls_without_titles() {
        let c = load(&SMTP).expect("config");
        let mail = c.mail.expect("mail on");
        assert_eq!(mail.smtp.host, "smtp.example.vn");
        assert_eq!(mail.smtp.tls, TlsMode::Starttls);
        assert_eq!(mail.smtp.port, 587);
        assert_eq!(mail.smtp.username, None);
        assert_eq!(mail.public_origin, "https://office.erpcons.vn");
        assert!(!mail.include_title);
        assert!(c.api.mail.enabled);
        assert!(!c.api.mail.include_title);
        assert_eq!(c.api.mail.daily_cap, 100);
        assert_eq!(c.api.mail.global_daily_cap, 2000);
    }

    #[test]
    fn mail_overrides_apply() {
        let mut vars = SMTP.to_vec();
        vars.extend([
            ("ERPCONS_SMTP_TLS", "tls"),
            ("ERPCONS_SMTP_USERNAME", "office"),
            ("ERPCONS_SMTP_PASSWORD", "smtp-secret"),
            ("ERPCONS_MAIL_INCLUDE_TITLE", "true"),
            ("ERPCONS_MAIL_DAILY_CAP", "20"),
            ("ERPCONS_MAIL_GLOBAL_DAILY_CAP", "500"),
            ("ERPCONS_RATE_LINK_BURST", "9"),
        ]);
        let c = load(&vars).expect("config");
        let mail = c.mail.expect("mail on");
        assert_eq!(mail.smtp.tls, TlsMode::Tls);
        assert_eq!(mail.smtp.port, 465);
        assert_eq!(mail.smtp.username.as_deref(), Some("office"));
        assert!(mail.include_title);
        assert!(c.api.mail.include_title);
        assert_eq!(c.api.mail.daily_cap, 20);
        assert_eq!(c.api.mail.global_daily_cap, 500);
        assert_eq!(c.api.link_rate.burst, 9);
    }

    #[test]
    fn mail_settings_are_checked_only_with_a_host() {
        assert!(load(&[("ERPCONS_SMTP_TLS", "bogus")]).is_ok());
        let with = |extra: (&'static str, &'static str)| {
            let mut vars = SMTP.to_vec();
            vars.push(extra);
            load(&vars).err()
        };
        assert_eq!(
            with(("ERPCONS_SMTP_TLS", "bogus")),
            Some(ConfigError::Invalid("ERPCONS_SMTP_TLS"))
        );
        assert_eq!(
            with(("ERPCONS_SMTP_USERNAME", "office")),
            Some(ConfigError::Missing("ERPCONS_SMTP_PASSWORD"))
        );
        assert_eq!(
            with(("ERPCONS_PUBLIC_ORIGIN", "office.erpcons.vn")),
            Some(ConfigError::Invalid("ERPCONS_PUBLIC_ORIGIN"))
        );
        assert_eq!(
            with(("ERPCONS_SMTP_PORT", "0")),
            Some(ConfigError::Invalid("ERPCONS_SMTP_PORT"))
        );
        assert_eq!(
            with(("ERPCONS_MAIL_DAILY_CAP", "-1")),
            Some(ConfigError::Invalid("ERPCONS_MAIL_DAILY_CAP"))
        );
        let no_from: Vec<_> = SMTP
            .into_iter()
            .filter(|(k, _)| *k != "ERPCONS_MAIL_FROM")
            .collect();
        assert_eq!(
            load(&no_from).err(),
            Some(ConfigError::Missing("ERPCONS_MAIL_FROM"))
        );
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
            ("ERPCONS_PI_JWKS_TTL_SECS", "60"),
            ("PI_AUDIENCES", "pi-platform,erpcons"),
            (
                "PI_ALLOWED_CLIENTS",
                " erpcons-office-web , erpcons-office-desktop,",
            ),
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
        assert_eq!(c.identity.jwks_ttl, Duration::from_secs(60));
        assert_eq!(c.identity.audiences, ["pi-platform", "erpcons"]);
        assert_eq!(
            c.identity.allowed_clients,
            ["erpcons-office-web", "erpcons-office-desktop"]
        );
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
            .filter(|(k, _)| *k != "PI_ISSUER")
            .collect();
        let err = ServerConfig::from_lookup(|name| vars.get(name).map(|v| (*v).to_owned()));
        assert_eq!(err.err(), Some(ConfigError::Missing("PI_ISSUER")));
    }

    #[test]
    fn the_link_key_is_required_and_checked() {
        let vars: HashMap<&str, &str> = REQUIRED
            .into_iter()
            .filter(|(k, _)| *k != "ERPCONS_LINK_KEY")
            .collect();
        let err = ServerConfig::from_lookup(|name| vars.get(name).map(|v| (*v).to_owned()));
        assert_eq!(err.err(), Some(ConfigError::Missing("ERPCONS_LINK_KEY")));
        for bad in [
            "change-me",
            "c2hvcnQ=",
            "ZXJwY29ucy1vZmZpY2UtZGV2LWxpbmsta2V5LTAwMDEx",
        ] {
            let err = load(&[("ERPCONS_LINK_KEY", bad)]).err().expect("invalid");
            assert_eq!(err, ConfigError::InvalidLinkKey, "{bad}");
            assert!(!err.to_string().contains(bad));
        }
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
            ("PI_ISSUER", "ftp://pi"),
            ("PI_ALLOWED_CLIENTS", " , "),
            ("PI_AUDIENCES", ","),
            ("ERPCONS_PI_JWKS_TTL_SECS", "0"),
            ("ERPCONS_PI_TIMEOUT_MS", "0"),
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
