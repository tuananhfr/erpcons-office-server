use sqlx::PgExecutor;

/// Bytes counted against `owner_id`: every version not purged, each distinct content once.
pub async fn used_bytes(db: impl PgExecutor<'_>, owner_id: &str) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT COALESCE(SUM(bytes), 0)::BIGINT FROM (
             SELECT DISTINCT v.sha256, v.bytes
             FROM document_versions v
             JOIN documents d ON d.id = v.document_id
             WHERE d.owner_id = $1 AND d.state <> 'PURGED'
         ) AS counted",
    )
    .bind(owner_id)
    .fetch_one(db)
    .await
}

/// Whether content `sha256` is already counted in [`used_bytes`] of `owner_id`, so storing it
/// again costs nothing.
pub async fn already_counted(
    db: impl PgExecutor<'_>,
    owner_id: &str,
    sha256: &str,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT EXISTS (
             SELECT 1 FROM document_versions v JOIN documents d ON d.id = v.document_id
             WHERE d.owner_id = $1 AND d.state <> 'PURGED' AND v.sha256 = $2)",
    )
    .bind(owner_id)
    .bind(sha256)
    .fetch_one(db)
    .await
}
