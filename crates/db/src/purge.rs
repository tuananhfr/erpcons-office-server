//! Queries of purging, by the job (plan §10: trash kept 30 days, then PURGE_PENDING, then PURGED)
//! or on the owner's request from the trash.
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

/// Objects nothing live uses: durable ones referenced only by documents being purged or purged,
/// and those whose last live version was deleted (`DELETE_PENDING`).
pub async fn dead_objects(
    db: impl PgExecutor<'_>,
    limit: i64,
) -> Result<Vec<DeadObject>, sqlx::Error> {
    sqlx::query_as(&format!(
        "SELECT o.id, o.opaque_key, o.sha256 FROM storage_objects o
         WHERE (o.durability_state = 'DELETE_PENDING'
                OR (o.durability_state = 'DURABLE'
                    AND EXISTS (
                        SELECT 1 FROM document_versions v JOIN documents d ON d.id = v.document_id
                        WHERE v.storage_object_id = o.id AND d.state = 'PURGE_PENDING')))
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
             WHERE o.id = $1 AND o.durability_state IN ('DURABLE', 'DELETE_PENDING')
               AND {NO_LIVE_REFERENCE})"
    ))
    .bind(id)
    .fetch_one(conn)
    .await
}

/// After a version was deleted: `DELETE_PENDING` when no live version uses object `id` any more,
/// so its bytes are deleted, by the request or else by the purge job. A save that stores the same
/// content again makes it `DURABLE` ([`crate::objects::upsert`]).
pub async fn release_if_unused(conn: &mut PgConnection, id: Uuid) -> Result<bool, sqlx::Error> {
    let result = sqlx::query(&format!(
        "UPDATE storage_objects o SET durability_state = 'DELETE_PENDING'
         WHERE o.id = $1 AND o.durability_state = 'DURABLE' AND {NO_LIVE_REFERENCE}"
    ))
    .bind(id)
    .execute(conn)
    .await?;
    Ok(result.rows_affected() == 1)
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
    finish_where(conn, None).await
}

/// As [`finish`], for `ids` only.
pub async fn finish_some(conn: &mut PgConnection, ids: &[Uuid]) -> Result<Vec<Uuid>, sqlx::Error> {
    finish_where(conn, Some(ids)).await
}

async fn finish_where(
    conn: &mut PgConnection,
    ids: Option<&[Uuid]>,
) -> Result<Vec<Uuid>, sqlx::Error> {
    sqlx::query_scalar(&format!(
        "UPDATE documents d SET state = 'PURGED', updated_at = now()
         WHERE d.state = 'PURGE_PENDING' AND ($1::uuid[] IS NULL OR d.id = ANY($1))
           AND NOT EXISTS (
             SELECT 1 FROM document_versions v JOIN storage_objects o ON o.id = v.storage_object_id
             WHERE v.document_id = d.id AND o.durability_state <> 'DELETED' AND {NO_LIVE_REFERENCE})
         RETURNING d.id"
    ))
    .bind(ids)
    .fetch_all(conn)
    .await
}

/// Trashed documents of `owner_id` among `ids` become `PURGE_PENDING`; returns those that did.
pub async fn mark_pending(
    db: impl PgExecutor<'_>,
    owner_id: &str,
    ids: &[Uuid],
) -> Result<Vec<Uuid>, sqlx::Error> {
    sqlx::query_scalar(
        "UPDATE documents SET state = 'PURGE_PENDING', updated_at = now()
         WHERE owner_id = $1 AND id = ANY($2) AND state = 'TRASHED'
         RETURNING id",
    )
    .bind(owner_id)
    .bind(ids)
    .fetch_all(db)
    .await
}

/// Deletes the permissions, invitations, share settings and link of purge-bound or purged documents
/// (`ids` only, when given). Their metadata stays as `PURGED`; who it was shared with does not.
pub async fn forget_sharing(
    conn: &mut PgConnection,
    ids: Option<&[Uuid]>,
) -> Result<u64, sqlx::Error> {
    let acl = sqlx::query(
        "DELETE FROM document_acl a USING documents d
         WHERE d.id = a.document_id AND d.state IN ('PURGE_PENDING', 'PURGED')
           AND ($1::uuid[] IS NULL OR d.id = ANY($1))",
    )
    .bind(ids)
    .execute(&mut *conn)
    .await?;
    let settings = sqlx::query(
        "DELETE FROM document_share_settings s USING documents d
         WHERE d.id = s.document_id AND d.state IN ('PURGE_PENDING', 'PURGED')
           AND ($1::uuid[] IS NULL OR d.id = ANY($1))",
    )
    .bind(ids)
    .execute(conn)
    .await?;
    Ok(acl.rows_affected() + settings.rows_affected())
}

/// As [`dead_objects`], limited to the content of `document_ids`.
pub async fn dead_objects_of(
    db: impl PgExecutor<'_>,
    document_ids: &[Uuid],
) -> Result<Vec<DeadObject>, sqlx::Error> {
    sqlx::query_as(&format!(
        "SELECT o.id, o.opaque_key, o.sha256 FROM storage_objects o
         WHERE o.durability_state = 'DURABLE'
           AND EXISTS (
               SELECT 1 FROM document_versions v
               WHERE v.storage_object_id = o.id AND v.document_id = ANY($1))
           AND {NO_LIVE_REFERENCE}
         ORDER BY o.id"
    ))
    .bind(document_ids)
    .fetch_all(db)
    .await
}

/// What a folder purge removed: the folders deleted and the documents now `PURGE_PENDING`.
#[derive(Clone, Debug, Default)]
pub struct FolderPurge {
    pub folders: Vec<Uuid>,
    pub documents: Vec<Uuid>,
}

/// Deletes the trashed folder `id` with the subfolders trashed together with it (same
/// `trashed_at`), and makes the documents trashed with them `PURGE_PENDING`. Items trashed on their
/// own before stay in the trash, moved to the root, since their folder goes. `None`: not trashed.
pub async fn purge_folder_tree(
    conn: &mut PgConnection,
    owner_id: &str,
    id: Uuid,
) -> Result<Option<FolderPurge>, sqlx::Error> {
    let folders: Vec<Uuid> = sqlx::query_scalar(
        "WITH RECURSIVE batch AS (
             SELECT id, trashed_at FROM folders WHERE id = $1 AND owner_id = $2 AND state = 'TRASHED'
             UNION
             SELECT f.id, f.trashed_at FROM folders f JOIN batch b ON f.parent_id = b.id
             WHERE f.state = 'TRASHED' AND f.trashed_at = b.trashed_at
         )
         SELECT id FROM batch",
    )
    .bind(id)
    .bind(owner_id)
    .fetch_all(&mut *conn)
    .await?;
    if folders.is_empty() {
        return Ok(None);
    }
    let documents: Vec<Uuid> = sqlx::query_scalar(
        "UPDATE documents d SET state = 'PURGE_PENDING', folder_id = NULL, updated_at = now()
         FROM folders f
         WHERE f.id = ANY($2) AND d.owner_id = $1 AND d.folder_id = f.id
           AND d.state = 'TRASHED' AND d.trashed_at = f.trashed_at
         RETURNING d.id",
    )
    .bind(owner_id)
    .bind(&folders)
    .fetch_all(&mut *conn)
    .await?;
    sqlx::query(
        "UPDATE documents SET folder_id = NULL WHERE owner_id = $1 AND folder_id = ANY($2)",
    )
    .bind(owner_id)
    .bind(&folders)
    .execute(&mut *conn)
    .await?;
    sqlx::query(
        "UPDATE folders SET parent_id = NULL
         WHERE owner_id = $1 AND parent_id = ANY($2) AND NOT (id = ANY($2))",
    )
    .bind(owner_id)
    .bind(&folders)
    .execute(&mut *conn)
    .await?;
    // One statement, so the foreign keys between the deleted folders are checked once at its end.
    sqlx::query("DELETE FROM folders WHERE owner_id = $1 AND id = ANY($2)")
        .bind(owner_id)
        .bind(&folders)
        .execute(conn)
        .await?;
    Ok(Some(FolderPurge { folders, documents }))
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
