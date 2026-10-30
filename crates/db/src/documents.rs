use sqlx::PgExecutor;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::ItemState;

/// A permission or pending invitation in force, or the public link, on document `d`.
const SHARED: &str = "(EXISTS (
         SELECT 1 FROM document_acl sa
         WHERE sa.document_id = d.id AND sa.revoked_at IS NULL
           AND (sa.expires_at IS NULL OR sa.expires_at > now()))
     OR EXISTS (
         SELECT 1 FROM document_share_settings sl
         WHERE sl.document_id = d.id AND sl.link_role IS NOT NULL
           AND (sl.link_expires_at IS NULL OR sl.link_expires_at > now())))";

/// Columns of [`DocumentRow`] over [`FROM`].
pub(crate) fn columns() -> String {
    format!(
        "d.id, d.owner_id, d.folder_id, d.title, d.kind, d.mime, d.state, d.etag,
         d.trashed_at, d.created_at, d.updated_at, v.seq AS version, v.bytes AS size,
         o.display_name AS owner_name, {SHARED} AS shared"
    )
}

/// A document `d` with its current version `v` and owner `o`; drafts (no version yet) never match.
pub(crate) const FROM: &str = "documents d
     JOIN document_versions v ON v.id = d.current_version_id
     JOIN actors o ON o.id = d.owner_id";

fn select() -> String {
    format!("SELECT {} FROM {FROM}", columns())
}

/// No `Debug`: the title and the owner's name are personal data and must not reach logs.
#[derive(Clone, sqlx::FromRow)]
pub struct DocumentRow {
    pub id: Uuid,
    pub owner_id: String,
    pub folder_id: Option<Uuid>,
    pub title: String,
    pub kind: String,
    pub mime: String,
    #[sqlx(try_from = "String")]
    pub state: ItemState,
    pub etag: String,
    pub trashed_at: Option<OffsetDateTime>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
    pub version: i32,
    pub size: i64,
    pub owner_name: String,
    pub shared: bool,
}

pub async fn get(db: impl PgExecutor<'_>, id: Uuid) -> Result<Option<DocumentRow>, sqlx::Error> {
    sqlx::query_as(&format!("{} WHERE d.id = $1", select()))
        .bind(id)
        .fetch_optional(db)
        .await
}

pub async fn sort_key(
    db: impl PgExecutor<'_>,
    owner_id: &str,
    id: Uuid,
) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar("SELECT lower(title) FROM documents WHERE id = $1 AND owner_id = $2")
        .bind(id)
        .bind(owner_id)
        .fetch_optional(db)
        .await
}

/// Active documents directly in `folder` (`None`: the root), by title then id, after `after`.
pub async fn list_in_folder(
    db: impl PgExecutor<'_>,
    owner_id: &str,
    folder: Option<Uuid>,
    after: Option<(&str, Uuid)>,
    limit: i64,
) -> Result<Vec<DocumentRow>, sqlx::Error> {
    sqlx::query_as(&format!(
        "{}
         WHERE d.owner_id = $1 AND d.folder_id IS NOT DISTINCT FROM $2 AND d.state = 'ACTIVE'
           AND ($3::text IS NULL OR (lower(d.title), d.id) > ($3::text, $4::uuid))
         ORDER BY lower(d.title), d.id
         LIMIT $5",
        select()
    ))
    .bind(owner_id)
    .bind(folder)
    .bind(after.map(|a| a.0))
    .bind(after.map(|a| a.1))
    .bind(limit)
    .fetch_all(db)
    .await
}

/// Top-level trashed documents (not trashed with their folder), in the order of
/// [`crate::folders::list_trash`] with rank 1.
pub async fn list_trash(
    db: impl PgExecutor<'_>,
    owner_id: &str,
    after: Option<(OffsetDateTime, i16, Uuid)>,
    limit: i64,
) -> Result<Vec<DocumentRow>, sqlx::Error> {
    sqlx::query_as(&format!(
        "{}
         WHERE d.owner_id = $1 AND d.state = 'TRASHED'
           AND NOT EXISTS (
               SELECT 1 FROM folders p
               WHERE p.id = d.folder_id AND p.state = 'TRASHED' AND p.trashed_at = d.trashed_at)
           AND ($2::timestamptz IS NULL OR d.trashed_at < $2
                OR (d.trashed_at = $2 AND ($3 < 1 OR ($3 = 1 AND d.id > $4))))
         ORDER BY d.trashed_at DESC, d.id
         LIMIT $5",
        select()
    ))
    .bind(owner_id)
    .bind(after.map(|a| a.0))
    .bind(after.map_or(0, |a| a.1))
    .bind(after.map(|a| a.2))
    .bind(limit)
    .fetch_all(db)
    .await
}

pub struct NewDocument<'a> {
    pub id: Uuid,
    pub owner_id: &'a str,
    pub folder_id: Option<Uuid>,
    pub title: &'a str,
    pub kind: &'a str,
    pub mime: &'a str,
}

/// Inserts a `DRAFT`; it becomes visible once [`set_current_version`] gives it a version.
pub async fn insert_draft(
    db: impl PgExecutor<'_>,
    doc: &NewDocument<'_>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO documents (id, owner_id, folder_id, title, kind, mime)
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(doc.id)
    .bind(doc.owner_id)
    .bind(doc.folder_id)
    .bind(doc.title)
    .bind(doc.kind)
    .bind(doc.mime)
    .execute(db)
    .await
    .map(drop)
}

/// Points the document at a new version and etag; a draft becomes active.
pub async fn set_current_version(
    db: impl PgExecutor<'_>,
    id: Uuid,
    version_id: Uuid,
    etag: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE documents SET
             state = CASE WHEN state = 'DRAFT' THEN 'ACTIVE' ELSE state END,
             current_version_id = $2, etag = $3, updated_at = now()
         WHERE id = $1",
    )
    .bind(id)
    .bind(version_id)
    .bind(etag)
    .execute(db)
    .await
    .map(drop)
}

pub struct DocumentChange<'a> {
    pub title: Option<&'a str>,
    /// `Some(None)` moves the document to the root.
    pub folder_id: Option<Option<Uuid>>,
}

pub async fn update(
    db: impl PgExecutor<'_>,
    id: Uuid,
    change: &DocumentChange<'_>,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query(
        "UPDATE documents SET
             title = COALESCE($2, title),
             folder_id = CASE WHEN $3 THEN $4 ELSE folder_id END,
             updated_at = now()
         WHERE id = $1 AND state = 'ACTIVE'",
    )
    .bind(id)
    .bind(change.title)
    .bind(change.folder_id.is_some())
    .bind(change.folder_id.flatten())
    .execute(db)
    .await?;
    Ok(result.rows_affected() == 1)
}

pub async fn trash(db: impl PgExecutor<'_>, id: Uuid) -> Result<bool, sqlx::Error> {
    let result = sqlx::query(
        "UPDATE documents SET state = 'TRASHED', trashed_at = now(), updated_at = now()
         WHERE id = $1 AND state = 'ACTIVE'",
    )
    .bind(id)
    .execute(db)
    .await?;
    Ok(result.rows_affected() == 1)
}

pub async fn restore(
    db: impl PgExecutor<'_>,
    id: Uuid,
    folder_id: Option<Uuid>,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query(
        "UPDATE documents SET state = 'ACTIVE', trashed_at = NULL, folder_id = $2, updated_at = now()
         WHERE id = $1 AND state = 'TRASHED'",
    )
    .bind(id)
    .bind(folder_id)
    .execute(db)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// Storage key, version and etag of the current content.
#[derive(Clone, Debug, sqlx::FromRow)]
pub struct ContentRef {
    pub opaque_key: String,
    pub version: i32,
    pub size: i64,
    pub etag: String,
}

pub async fn content_ref(
    db: impl PgExecutor<'_>,
    id: Uuid,
) -> Result<Option<ContentRef>, sqlx::Error> {
    sqlx::query_as(
        "SELECT o.opaque_key, v.seq AS version, v.bytes AS size, d.etag
         FROM documents d
         JOIN document_versions v ON v.id = d.current_version_id
         JOIN storage_objects o ON o.id = v.storage_object_id
         WHERE d.id = $1",
    )
    .bind(id)
    .fetch_optional(db)
    .await
}
