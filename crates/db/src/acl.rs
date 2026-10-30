//! Permissions on documents (`document_acl`): granted to an actor, or pending for an email until an
//! account with that verified email signs in. Revoking keeps the row with `revoked_at` set.
use sqlx::{PgConnection, PgExecutor};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::documents::{self, DocumentRow};

/// One unrevoked permission per person and one pending invitation per address on a document.
pub const ACTOR_KEY: &str = "document_acl_actor_key";
pub const EMAIL_KEY: &str = "document_acl_email_key";

/// A write lost a race with another one giving the same person access to the same document.
pub fn is_collision(err: &sqlx::Error) -> bool {
    crate::is_unique_violation(err, ACTOR_KEY) || crate::is_unique_violation(err, EMAIL_KEY)
}

/// Rows still in force; an expired permission counts as removed.
const IN_FORCE: &str = "a.revoked_at IS NULL AND (a.expires_at IS NULL OR a.expires_at > now())";

/// No `Debug`: names and emails are personal data.
#[derive(Clone, sqlx::FromRow)]
pub struct PermissionRow {
    pub id: Uuid,
    pub actor_id: Option<String>,
    pub display_name: Option<String>,
    pub email: Option<String>,
    pub role: String,
    pub created_at: OffsetDateTime,
    pub expires_at: Option<OffsetDateTime>,
}

/// The address shown is the one invited, never the account's current email at PI: sharing must not
/// tell a sharer which address stands behind an account.
const PERMISSION: &str = "SELECT a.id, a.actor_id, ac.display_name, a.email_norm AS email, a.role,
         a.created_at, a.expires_at
     FROM document_acl a
     LEFT JOIN actors ac ON ac.id = a.actor_id";

/// Permissions in force on `document_id`, oldest first, at most `limit`; only those of `only_actor`
/// when given.
pub async fn list(
    db: impl PgExecutor<'_>,
    document_id: Uuid,
    only_actor: Option<&str>,
    limit: i64,
) -> Result<Vec<PermissionRow>, sqlx::Error> {
    sqlx::query_as(&format!(
        "{PERMISSION} WHERE a.document_id = $1 AND {IN_FORCE}
             AND ($2::text IS NULL OR a.actor_id = $2)
         ORDER BY a.created_at, a.id LIMIT $3"
    ))
    .bind(document_id)
    .bind(only_actor)
    .bind(limit)
    .fetch_all(db)
    .await
}

/// How many of [`list`] there are without the limit.
pub async fn count(
    db: impl PgExecutor<'_>,
    document_id: Uuid,
    only_actor: Option<&str>,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(&format!(
        "SELECT count(*) FROM document_acl a WHERE a.document_id = $1 AND {IN_FORCE}
             AND ($2::text IS NULL OR a.actor_id = $2)"
    ))
    .bind(document_id)
    .bind(only_actor)
    .fetch_one(db)
    .await
}

/// Distinct addresses `granted_by` gave a permission or an invitation in the last `secs` seconds,
/// removed ones included.
pub async fn recipients_since(
    db: impl PgExecutor<'_>,
    granted_by: &str,
    secs: i64,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT count(DISTINCT email_norm) FROM document_acl
         WHERE granted_by = $1 AND created_at > now() - make_interval(secs => $2)",
    )
    .bind(granted_by)
    .bind(secs as f64)
    .fetch_one(db)
    .await
}

/// One permission in force on `document_id`.
pub async fn get(
    db: impl PgExecutor<'_>,
    document_id: Uuid,
    id: Uuid,
) -> Result<Option<PermissionRow>, sqlx::Error> {
    sqlx::query_as(&format!(
        "{PERMISSION} WHERE a.document_id = $1 AND a.id = $2 AND {IN_FORCE}"
    ))
    .bind(document_id)
    .bind(id)
    .fetch_optional(db)
    .await
}

/// A row not revoked (perhaps expired), which blocks a second one for the same person.
#[derive(Clone, Debug, sqlx::FromRow)]
pub struct Existing {
    pub id: Uuid,
    pub role: String,
    pub expired: bool,
    pub expires_at: Option<OffsetDateTime>,
}

/// The unrevoked row of `actor_id` on `document_id`.
pub async fn existing_for_actor(
    conn: &mut PgConnection,
    document_id: Uuid,
    actor_id: &str,
) -> Result<Option<Existing>, sqlx::Error> {
    sqlx::query_as(
        "SELECT id, role, (expires_at IS NOT NULL AND expires_at <= now()) AS expired, expires_at
         FROM document_acl
         WHERE document_id = $1 AND actor_id = $2 AND revoked_at IS NULL",
    )
    .bind(document_id)
    .bind(actor_id)
    .fetch_optional(conn)
    .await
}

/// The unrevoked pending invitation of `email_norm` on `document_id`.
pub async fn existing_pending(
    conn: &mut PgConnection,
    document_id: Uuid,
    email_norm: &str,
) -> Result<Option<Existing>, sqlx::Error> {
    sqlx::query_as(
        "SELECT id, role, (expires_at IS NOT NULL AND expires_at <= now()) AS expired, expires_at
         FROM document_acl
         WHERE document_id = $1 AND email_norm = $2 AND actor_id IS NULL AND revoked_at IS NULL",
    )
    .bind(document_id)
    .bind(email_norm)
    .fetch_optional(conn)
    .await
}

pub struct NewPermission<'a> {
    pub id: Uuid,
    pub document_id: Uuid,
    pub actor_id: Option<&'a str>,
    pub email_norm: Option<&'a str>,
    pub role: &'a str,
    pub granted_by: &'a str,
    pub expires_at: Option<OffsetDateTime>,
}

pub async fn insert(conn: &mut PgConnection, p: &NewPermission<'_>) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO document_acl
             (id, document_id, actor_id, email_norm, role, granted_by, expires_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(p.id)
    .bind(p.document_id)
    .bind(p.actor_id)
    .bind(p.email_norm)
    .bind(p.role)
    .bind(p.granted_by)
    .bind(p.expires_at)
    .execute(conn)
    .await
    .map(drop)
}

/// Gives an existing row `role` again (a repeated invitation), ending at `expires_at`.
pub async fn renew(
    conn: &mut PgConnection,
    id: Uuid,
    role: &str,
    expires_at: Option<OffsetDateTime>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE document_acl SET role = $2, expires_at = $3, updated_at = now() WHERE id = $1",
    )
    .bind(id)
    .bind(role)
    .bind(expires_at)
    .execute(conn)
    .await
    .map(drop)
}

pub struct PermissionChange<'a> {
    pub role: Option<&'a str>,
    /// `Some(None)` removes the expiry.
    pub expires_at: Option<Option<OffsetDateTime>>,
}

/// Changes a permission in force; `false` when there is none.
pub async fn update(
    conn: &mut PgConnection,
    document_id: Uuid,
    id: Uuid,
    change: &PermissionChange<'_>,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query(&format!(
        "UPDATE document_acl a SET
             role = COALESCE($3, a.role),
             expires_at = CASE WHEN $4 THEN $5 ELSE a.expires_at END,
             updated_at = now()
         WHERE a.document_id = $1 AND a.id = $2 AND {IN_FORCE}"
    ))
    .bind(document_id)
    .bind(id)
    .bind(change.role)
    .bind(change.expires_at.is_some())
    .bind(change.expires_at.flatten())
    .execute(conn)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// Revokes a permission in force; `false` when there is none.
pub async fn revoke(
    conn: &mut PgConnection,
    document_id: Uuid,
    id: Uuid,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query(&format!(
        "UPDATE document_acl a SET revoked_at = now(), updated_at = now()
         WHERE a.document_id = $1 AND a.id = $2 AND {IN_FORCE}"
    ))
    .bind(document_id)
    .bind(id)
    .execute(conn)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// Gives `actor_id` the invitations pending for its verified `email_norm`. One it cannot take (it owns
/// the document, or already has a permission in force there) is revoked, keeping that permission; an
/// expired permission of its own gives way to the invitation.
pub async fn attach_pending(
    conn: &mut PgConnection,
    actor_id: &str,
    email_norm: &str,
) -> Result<u64, sqlx::Error> {
    sqlx::query(
        "UPDATE document_acl a SET revoked_at = now(), updated_at = now()
         WHERE a.actor_id = $1 AND a.revoked_at IS NULL
           AND a.expires_at IS NOT NULL AND a.expires_at <= now()
           AND EXISTS (SELECT 1 FROM document_acl p
                       WHERE p.document_id = a.document_id AND p.actor_id IS NULL
                         AND p.email_norm = $2 AND p.revoked_at IS NULL)",
    )
    .bind(actor_id)
    .bind(email_norm)
    .execute(&mut *conn)
    .await?;
    sqlx::query(
        "UPDATE document_acl p SET revoked_at = now(), updated_at = now()
         WHERE p.actor_id IS NULL AND p.email_norm = $2 AND p.revoked_at IS NULL
           AND (EXISTS (SELECT 1 FROM documents d WHERE d.id = p.document_id AND d.owner_id = $1)
                OR EXISTS (SELECT 1 FROM document_acl a
                           WHERE a.document_id = p.document_id AND a.actor_id = $1
                             AND a.revoked_at IS NULL))",
    )
    .bind(actor_id)
    .bind(email_norm)
    .execute(&mut *conn)
    .await?;
    let result = sqlx::query(
        "UPDATE document_acl SET actor_id = $1, updated_at = now()
         WHERE actor_id IS NULL AND email_norm = $2 AND revoked_at IS NULL",
    )
    .bind(actor_id)
    .bind(email_norm)
    .execute(conn)
    .await?;
    Ok(result.rows_affected())
}

/// No `Debug`: holds the title.
#[derive(Clone, sqlx::FromRow)]
pub struct SharedRow {
    #[sqlx(flatten)]
    pub document: DocumentRow,
    pub grant_id: Uuid,
    pub grant_role: String,
    pub shared_at: OffsetDateTime,
    pub unseen: bool,
    pub viewers_can_download: bool,
    pub editors_can_share: bool,
}

const SHARED_WITH: &str = "a.actor_id = $1 AND a.revoked_at IS NULL
           AND (a.expires_at IS NULL OR a.expires_at > now())
           AND d.state = 'ACTIVE' AND d.owner_id <> $1";

/// Active documents shared with `actor_id`, most recently shared first, after `(shared_at, grant id)`.
pub async fn shared_with(
    db: impl PgExecutor<'_>,
    actor_id: &str,
    after: Option<(OffsetDateTime, Uuid)>,
    limit: i64,
) -> Result<Vec<SharedRow>, sqlx::Error> {
    sqlx::query_as(&format!(
        "SELECT {}, a.id AS grant_id, a.role AS grant_role, a.created_at AS shared_at,
             (a.seen_at IS NULL) AS unseen,
             COALESCE(s.viewers_can_download, true) AS viewers_can_download,
             COALESCE(s.editors_can_share, true) AS editors_can_share
         FROM document_acl a
         JOIN ({}) ON d.id = a.document_id
         LEFT JOIN document_share_settings s ON s.document_id = d.id
         WHERE {SHARED_WITH}
           AND ($2::timestamptz IS NULL OR (a.created_at, a.id) < ($2, $3::uuid))
         ORDER BY a.created_at DESC, a.id DESC
         LIMIT $4",
        documents::columns(),
        documents::FROM,
    ))
    .bind(actor_id)
    .bind(after.map(|a| a.0))
    .bind(after.map(|a| a.1))
    .bind(limit)
    .fetch_all(db)
    .await
}

/// How many of [`shared_with`] are not marked seen.
pub async fn unseen_count(db: impl PgExecutor<'_>, actor_id: &str) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(&format!(
        "SELECT count(*) FROM document_acl a JOIN documents d ON d.id = a.document_id
         WHERE {SHARED_WITH} AND a.seen_at IS NULL"
    ))
    .bind(actor_id)
    .fetch_one(db)
    .await
}

/// Marks the permission of `actor_id` on `document_id` seen; `false` when it has none in force.
pub async fn mark_seen(
    db: impl PgExecutor<'_>,
    actor_id: &str,
    document_id: Uuid,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar(&format!(
        "UPDATE document_acl a SET seen_at = COALESCE(a.seen_at, now())
         FROM documents d
         WHERE d.id = a.document_id AND a.document_id = $2 AND {SHARED_WITH}
         RETURNING true"
    ))
    .bind(actor_id)
    .bind(document_id)
    .fetch_optional(db)
    .await
    .map(|row: Option<bool>| row.is_some())
}
