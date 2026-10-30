//! A document together with what decides who may use it: the caller's permission and the share settings.
//! Read on every request (no cache), so a removed permission is refused at the next one.
use sqlx::{PgConnection, PgExecutor};
use uuid::Uuid;

use crate::documents::{self, DocumentRow};

/// No `Debug`: holds the title.
#[derive(Clone, sqlx::FromRow)]
pub struct AccessRow {
    #[sqlx(flatten)]
    pub document: DocumentRow,
    /// The role of a permission in force for the asking actor; `None` for the owner and for strangers.
    pub grant_role: Option<String>,
    /// When that permission ends; `None` when it does not, or there is none.
    pub grant_expires_at: Option<time::OffsetDateTime>,
    pub editors_can_share: bool,
    pub viewers_can_download: bool,
}

fn query(lock: bool) -> String {
    format!(
        "SELECT {}, g.role AS grant_role, g.expires_at AS grant_expires_at,
             COALESCE(s.editors_can_share, true) AS editors_can_share,
             COALESCE(s.viewers_can_download, true) AS viewers_can_download
         FROM {} LEFT JOIN document_share_settings s ON s.document_id = d.id
         LEFT JOIN LATERAL (
             SELECT role, expires_at FROM document_acl
             WHERE document_id = d.id AND actor_id = $2 AND revoked_at IS NULL
               AND (expires_at IS NULL OR expires_at > now())) g ON true
         WHERE d.id = $1{}",
        documents::columns(),
        documents::FROM,
        if lock { " FOR UPDATE OF d" } else { "" }
    )
}

/// `id` as `actor_id` sees it.
pub async fn of_actor(
    db: impl PgExecutor<'_>,
    id: Uuid,
    actor_id: &str,
) -> Result<Option<AccessRow>, sqlx::Error> {
    sqlx::query_as(&query(false))
        .bind(id)
        .bind(actor_id)
        .fetch_optional(db)
        .await
}

/// As [`of_actor`], with the document row locked until the transaction ends: sharing changes of one
/// document then run one after the other, and a save sees the permissions as they are at its commit.
pub async fn of_actor_for_update(
    conn: &mut PgConnection,
    id: Uuid,
    actor_id: &str,
) -> Result<Option<AccessRow>, sqlx::Error> {
    sqlx::query_as(&query(true))
        .bind(id)
        .bind(actor_id)
        .fetch_optional(conn)
        .await
}

/// The document whose public link has the token hashing to `token_hash`, while the link is on.
pub async fn of_link(
    db: impl PgExecutor<'_>,
    token_hash: &str,
) -> Result<Option<AccessRow>, sqlx::Error> {
    sqlx::query_as(&format!(
        "SELECT {}, NULL::text AS grant_role, NULL::timestamptz AS grant_expires_at,
             s.editors_can_share, s.viewers_can_download
         FROM {} JOIN document_share_settings s ON s.document_id = d.id
         WHERE s.link_token_hash = $1 AND s.link_role IS NOT NULL
           AND (s.link_expires_at IS NULL OR s.link_expires_at > now())",
        documents::columns(),
        documents::FROM,
    ))
    .bind(token_hash)
    .fetch_optional(db)
    .await
}
