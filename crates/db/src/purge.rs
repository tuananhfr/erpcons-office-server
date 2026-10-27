//! Queries of the purge job (plan §10: trash kept 30 days, then PURGE_PENDING, then PURGED).
use sqlx::{PgConnection, PgExecutor};
use uuid::Uuid;

/// Moves documents trashed more than `retention_secs` ago to `PURGE_PENDING`.
pub async fn mark_expired(
    db: impl PgExecutor<'_>,
    retention_secs: i64,
) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(
        "UPDATE documents SET state = 'PURGE_PENDING', updated_at = now()
         WHERE state = 'TRASHED' AND trashed_at < now() - make_interval(secs => $1)",
    )
    .bind(retention_secs as f64)
    .execute(db)
    .await?;
    Ok(result.rows_affected())
}

/// An object only purge-bound documents still use.
#[derive(Clone, Debug, sqlx::FromRow)]
pub struct DeadObject {
    pub id: Uuid,
    pub opaque_key: String,
    pub sha256: String,
}

const NO_LIVE_REFERENCE: &str = "NOT EXISTS (
        SELECT 1 FROM document_versions v JOIN documents d ON d.id = v.document_id
        WHERE v.storage_object_id = o.id AND d.state NOT IN ('PURGE_PENDING', 'PURGED'))";

/// Durable objects referenced only by documents being purged or purged.
pub async fn dead_objects(
    db: impl PgExecutor<'_>,
    limit: i64,
) -> Result<Vec<DeadObject>, sqlx::Error> {
    sqlx::query_as(&format!(
        "SELECT o.id, o.opaque_key, o.sha256 FROM storage_objects o
         WHERE o.durability_state = 'DURABLE'
           AND EXISTS (
               SELECT 1 FROM document_versions v JOIN documents d ON d.id = v.document_id
               WHERE v.storage_object_id = o.id AND d.state = 'PURGE_PENDING')
           AND {NO_LIVE_REFERENCE}
         ORDER BY o.id
         LIMIT $1"
    ))
    .bind(limit)
    .fetch_all(db)
    .await
}

/// Re-checks, under the object's lock, that nothing live uses `id`.
pub async fn still_dead(conn: &mut PgConnection, id: Uuid) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar(&format!(
        "SELECT EXISTS (
             SELECT 1 FROM storage_objects o
             WHERE o.id = $1 AND o.durability_state = 'DURABLE' AND {NO_LIVE_REFERENCE})"
    ))
    .bind(id)
    .fetch_one(conn)
    .await
}

pub async fn mark_deleted(db: impl PgExecutor<'_>, id: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE storage_objects SET durability_state = 'DELETED' WHERE id = $1")
        .bind(id)
        .execute(db)
        .await
        .map(drop)
}

/// `PURGE_PENDING` documents none of whose content is still waiting for deletion become `PURGED`.
/// Content shared with a live document is kept for that document.
pub async fn finish(conn: &mut PgConnection) -> Result<Vec<Uuid>, sqlx::Error> {
    sqlx::query_scalar(&format!(
        "UPDATE documents d SET state = 'PURGED', updated_at = now()
         WHERE d.state = 'PURGE_PENDING' AND NOT EXISTS (
             SELECT 1 FROM document_versions v JOIN storage_objects o ON o.id = v.storage_object_id
             WHERE v.document_id = d.id AND o.durability_state <> 'DELETED' AND {NO_LIVE_REFERENCE})
         RETURNING d.id"
    ))
    .fetch_all(conn)
    .await
}

/// Deletes trashed folders past retention that hold nothing but purged documents, deepest first
/// (one level per call). Purged documents in them move to the root so the folder can go.
pub async fn delete_expired_folders(
    conn: &mut PgConnection,
    retention_secs: i64,
) -> Result<Vec<Uuid>, sqlx::Error> {
    sqlx::query(
        "UPDATE documents d SET folder_id = NULL
         FROM folders f
         WHERE d.folder_id = f.id AND d.state IN ('PURGE_PENDING', 'PURGED')
           AND f.state = 'TRASHED' AND f.trashed_at < now() - make_interval(secs => $1)",
    )
    .bind(retention_secs as f64)
    .execute(&mut *conn)
    .await?;
    sqlx::query_scalar(
        "DELETE FROM folders f
         WHERE f.state = 'TRASHED' AND f.trashed_at < now() - make_interval(secs => $1)
           AND NOT EXISTS (SELECT 1 FROM folders c WHERE c.parent_id = f.id)
           AND NOT EXISTS (SELECT 1 FROM documents d WHERE d.folder_id = f.id)
         RETURNING f.id",
    )
    .bind(retention_secs as f64)
    .fetch_all(conn)
    .await
}
