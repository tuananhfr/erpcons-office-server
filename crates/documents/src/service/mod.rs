mod audit;
mod content;
mod documents;
mod folders;
mod listing;
mod purge;
mod replay;
mod staging;

use std::sync::Arc;

use office_db::{PgConnection, PgPool, Postgres, Transaction, actors};
use office_identity::Actor;
use office_storage::StorageAdapter;

pub(crate) use audit::{Subject, system as audit_system};
pub use content::{Content, CreateDocument};
pub use documents::DocumentPatch;
pub use folders::FolderPatch;
pub use staging::Staged;

use crate::{DocError, FailPoints, QuotaConfig, intake::ZipLimits};

/// Who asks, and the request id their audit rows carry.
#[derive(Clone, Copy)]
pub struct Caller<'a> {
    pub actor: &'a Actor,
    pub trace_id: &'a str,
}

impl Caller<'_> {
    pub(crate) fn owner(&self) -> &str {
        &self.actor.actor_id
    }
}

/// The document service: folders, documents, versions, trash, and the purge and orphan jobs.
pub struct Documents {
    pub(crate) db: PgPool,
    pub(crate) storage: Arc<dyn StorageAdapter>,
    quota: QuotaConfig,
    zip_limits: ZipLimits,
    fail_points: FailPoints,
}

impl Documents {
    pub fn new(db: PgPool, storage: Arc<dyn StorageAdapter>, quota: QuotaConfig) -> Self {
        Self {
            db,
            storage,
            quota,
            zip_limits: ZipLimits::DEFAULT,
            fail_points: FailPoints::default(),
        }
    }

    pub fn quota(&self) -> QuotaConfig {
        self.quota
    }

    #[cfg(feature = "testing")]
    pub fn fail_points(&self) -> &FailPoints {
        &self.fail_points
    }

    pub(crate) async fn begin(&self) -> Result<Transaction<'static, Postgres>, DocError> {
        self.db.begin().await.map_err(DocError::db)
    }

    /// Creates or refreshes the caller's actor row, which every owned row references. Inside a
    /// transaction this also locks the row, serializing the caller's quota checks.
    async fn touch_actor(&self, conn: &mut PgConnection, c: &Caller<'_>) -> Result<(), DocError> {
        actors::upsert_seen(conn, &c.actor.actor_id, &c.actor.display_name)
            .await
            .map(drop)
            .map_err(DocError::db)
    }
}
