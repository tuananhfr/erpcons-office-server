use std::{net::IpAddr, sync::Arc};

use office_db::PgPool;
use office_documents::Documents;
use office_identity::IdentityVerifier;
use office_storage::StorageAdapter;

use crate::{ApiConfig, rate_limit::KeyedLimiter};

/// Everything a handler may use; cheap to clone.
#[derive(Clone)]
pub struct AppState {
    pub(crate) db: PgPool,
    pub(crate) storage: Arc<dyn StorageAdapter>,
    pub(crate) identity: Arc<dyn IdentityVerifier>,
    pub(crate) docs: Arc<Documents>,
    pub(crate) config: Arc<ApiConfig>,
    /// `None` collects requests whose address is unknown into one shared bucket.
    pub(crate) ip_limiter: Arc<KeyedLimiter<Option<IpAddr>>>,
    pub(crate) actor_limiter: Arc<KeyedLimiter<String>>,
}

impl AppState {
    pub fn new(
        db: PgPool,
        storage: Arc<dyn StorageAdapter>,
        identity: Arc<dyn IdentityVerifier>,
        config: ApiConfig,
    ) -> Self {
        let docs = Arc::new(Documents::new(db.clone(), storage.clone(), config.quota));
        Self {
            db,
            storage,
            identity,
            docs,
            ip_limiter: Arc::new(KeyedLimiter::new(config.ip_rate)),
            actor_limiter: Arc::new(KeyedLimiter::new(config.actor_rate)),
            config: Arc::new(config),
        }
    }

    /// The document service behind the routes; the server also runs its jobs.
    pub fn documents(&self) -> &Arc<Documents> {
        &self.docs
    }
}
