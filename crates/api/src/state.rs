use std::sync::Arc;

use office_db::PgPool;
use office_documents::Documents;
use office_identity::IdentityVerifier;
use office_storage::StorageAdapter;

use crate::{
    ApiConfig, RateConfig,
    rate_limit::{ClientKey, ConcurrencyLimiter, KeyedLimiter},
};

/// One audit row per client and outcome per minute for refused tokens, enough to see an attack
/// without letting it fill the table.
const DENIAL_AUDIT_RATE: RateConfig = RateConfig {
    burst: 1,
    per_second: 1.0 / 60.0,
};

/// Everything a handler may use; cheap to clone.
#[derive(Clone)]
pub struct AppState {
    pub(crate) db: PgPool,
    pub(crate) storage: Arc<dyn StorageAdapter>,
    pub(crate) identity: Arc<dyn IdentityVerifier>,
    pub(crate) docs: Arc<Documents>,
    pub(crate) config: Arc<ApiConfig>,
    pub(crate) ip_limiter: Arc<KeyedLimiter<ClientKey>>,
    pub(crate) actor_limiter: Arc<KeyedLimiter<String>>,
    /// Token checks the verifier cannot answer from its cache, per client.
    pub(crate) verify_limiter: Arc<KeyedLimiter<ClientKey>>,
    /// Keyed by client and whether PI was unavailable (else the token was refused).
    pub(crate) denial_audit: Arc<KeyedLimiter<(ClientKey, bool)>>,
    pub(crate) upload_slots: Arc<ConcurrencyLimiter>,
    /// Public view links, per client.
    pub(crate) link_limiter: Arc<KeyedLimiter<ClientKey>>,
}

impl AppState {
    pub fn new(
        db: PgPool,
        storage: Arc<dyn StorageAdapter>,
        identity: Arc<dyn IdentityVerifier>,
        config: ApiConfig,
    ) -> Self {
        let docs = Arc::new(
            Documents::new(db.clone(), storage.clone(), config.quota)
                .with_mail(config.mail)
                .with_link_key(config.link_key.clone()),
        );
        Self {
            db,
            storage,
            identity,
            docs,
            ip_limiter: Arc::new(KeyedLimiter::new(config.ip_rate)),
            actor_limiter: Arc::new(KeyedLimiter::new(config.actor_rate)),
            verify_limiter: Arc::new(KeyedLimiter::new(config.verify_rate)),
            denial_audit: Arc::new(KeyedLimiter::new(DENIAL_AUDIT_RATE)),
            upload_slots: Arc::new(ConcurrencyLimiter::new(config.max_concurrent_uploads)),
            link_limiter: Arc::new(KeyedLimiter::new(config.link_rate)),
            config: Arc::new(config),
        }
    }

    /// The document service behind the routes; the server also runs its jobs.
    pub fn documents(&self) -> &Arc<Documents> {
        &self.docs
    }
}
