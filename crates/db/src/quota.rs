use sqlx::PgPool;

/// Bytes counted against `owner_id`: every version not purged, each distinct content once.
pub async fn used_bytes(pool: &PgPool, owner_id: &str) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT COALESCE(SUM(bytes), 0)::BIGINT FROM (
             SELECT DISTINCT v.sha256, v.bytes
             FROM document_versions v
             JOIN documents d ON d.id = v.document_id
             WHERE d.owner_id = $1 AND d.state <> 'PURGED'
         ) AS counted",
    )
    .bind(owner_id)
    .fetch_one(pool)
    .await
}
