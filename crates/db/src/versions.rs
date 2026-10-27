use sqlx::PgExecutor;
use time::OffsetDateTime;
use uuid::Uuid;

/// No `Debug`: the author name is personal data.
#[derive(Clone, sqlx::FromRow)]
pub struct VersionRow {
    pub seq: i32,
    pub bytes: i64,
    pub sha256: String,
    pub author_name_snapshot: String,
    pub created_at: OffsetDateTime,
}

pub struct NewVersion<'a> {
    pub id: Uuid,
    pub document_id: Uuid,
    pub seq: i32,
    pub storage_object_id: Uuid,
    pub sha256: &'a str,
    pub bytes: i64,
    pub author_id: &'a str,
    pub author_name: &'a str,
}

pub async fn insert(db: impl PgExecutor<'_>, v: &NewVersion<'_>) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO document_versions
             (id, document_id, seq, storage_object_id, sha256, bytes, author_id, author_name_snapshot)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(v.id)
    .bind(v.document_id)
    .bind(v.seq)
    .bind(v.storage_object_id)
    .bind(v.sha256)
    .bind(v.bytes)
    .bind(v.author_id)
    .bind(v.author_name)
    .execute(db)
    .await
    .map(drop)
}

/// Versions newest first, older than `before` when given.
pub async fn list(
    db: impl PgExecutor<'_>,
    document_id: Uuid,
    before: Option<i32>,
    limit: i64,
) -> Result<Vec<VersionRow>, sqlx::Error> {
    sqlx::query_as(
        "SELECT seq, bytes, sha256, author_name_snapshot, created_at FROM document_versions
         WHERE document_id = $1 AND ($2::int IS NULL OR seq < $2)
         ORDER BY seq DESC
         LIMIT $3",
    )
    .bind(document_id)
    .bind(before)
    .bind(limit)
    .fetch_all(db)
    .await
}
