mod access;
mod actor_sync;
mod audit;
mod content;
mod documents;
mod folders;
mod grants;
mod invite_mail;
mod links;
mod listing;
mod permissions;
mod purge;
mod replay;
mod share_settings;
mod shared;
mod staging;
mod versions;

use std::sync::Arc;

use office_db::{PgConnection, PgPool, Postgres, Transaction, actors};
use office_identity::Actor;
use office_mail::MailPolicy;
use office_storage::StorageAdapter;

pub(crate) use audit::{Subject, system as audit_system};
pub use content::{Content, CreateDocument};
pub use documents::DocumentPatch;
pub use folders::FolderPatch;
pub use permissions::{AddPermissions, PermissionPatch};
pub use staging::Staged;

use crate::{DocError, FailPoints, LinkKey, QuotaConfig, intake::ZipLimits};

/// Who asks, and the request id their audit rows carry.
#[derive(Clone, Copy)]
pub struct Caller<'a> {
    pub actor: &'a Actor,
    pub trace_id: &'a str,
}

impl Caller<'_> {
    /// The caller's id. Not the owner of what it acts on: a shared document's owner is someone else.
    pub(crate) fn actor_id(&self) -> &str {
        &self.actor.actor_id
    }
}

/// The document service: folders, documents, versions, trash, sharing, and the purge and orphan jobs.
pub struct Documents {
    pub(crate) db: PgPool,
    pub(crate) storage: Arc<dyn StorageAdapter>,
    quota: QuotaConfig,
    zip_limits: ZipLimits,
    fail_points: FailPoints,
    mail: MailPolicy,
    /// Seals link tokens; without it no link can be turned on.
    link_key: Option<LinkKey>,
}

impl Documents {
    pub fn new(db: PgPool, storage: Arc<dyn StorageAdapter>, quota: QuotaConfig) -> Self {
        Self {
            db,
            storage,
            quota,
            zip_limits: ZipLimits::DEFAULT,
            fail_points: FailPoints::default(),
            mail: MailPolicy::default(),
            link_key: None,
        }
    }

    /// Queue notification mail as `mail` says (nothing is queued by default).
    pub fn with_mail(mut self, mail: MailPolicy) -> Self {
        self.mail = mail;
        self
    }

    /// The key link tokens are sealed under (`ERPCONS_LINK_KEY`).
    pub fn with_link_key(mut self, key: Option<LinkKey>) -> Self {
        self.link_key = key;
        self
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

    /// Creates or refreshes the caller's actor row outside any transaction, for rows that only
    /// reference it (who granted a permission).
    async fn ensure_caller(&self, c: &Caller<'_>) -> Result<(), DocError> {
        actors::upsert_seen(&self.db, c.actor_id(), &c.actor.display_name)
            .await
            .map(drop)
            .map_err(DocError::db)
    }

    /// Creates or refreshes the caller's actor row, which every row it writes references, and locks
    /// the row of `owner_id`, serializing that owner's quota checks (lock order: actor row first).
    /// Only the owner's row is locked in the transaction, so two people saving each other's
    /// documents cannot deadlock on actor rows.
    async fn touch_actor(
        &self,
        conn: &mut PgConnection,
        c: &Caller<'_>,
        owner_id: &str,
    ) -> Result<(), DocError> {
        if owner_id == c.actor_id() {
            return actors::upsert_seen(conn, c.actor_id(), &c.actor.display_name)
                .await
                .map(drop)
                .map_err(DocError::db);
        }
        self.ensure_caller(c).await?;
        if actors::lock(conn, owner_id).await.map_err(DocError::db)? {
            Ok(())
        } else {
            Err(DocError::NotFound)
        }
    }
}
