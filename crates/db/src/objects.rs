use sqlx::PgExecutor;
use uuid::Uuid;

/// The filesystem location seeded by migration 0001.
pub const FS_LOCATION_ID: Uuid = Uuid::from_u128(1);

/// The row of stored content `sha256`, created or reused. A row whose bytes a purge deleted
/// becomes durable again: the caller has just stored the bytes anew.
pub async fn upsert(
    db: impl PgExecutor<'_>,
    opaque_key: &str,
    sha256: &str,
    size: i64,
) -> Result<Uuid, sqlx::Error> {
    sqlx::query_scalar(
        "INSERT INTO storage_objects (id, location_id, opaque_key, sha256, size)
         VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT (location_id, sha256) DO UPDATE SET durability_state = 'DURABLE'
         RETURNING id",
    )
    .bind(Uuid::now_v7())
    .bind(FS_LOCATION_ID)
    .bind(opaque_key)
    .bind(sha256)
    .bind(size)
    .fetch_one(db)
    .await
}

/// Those of `keys` that have a row whose bytes must be kept (not `DELETED`).
pub async fn kept_keys(
    db: impl PgExecutor<'_>,
    keys: &[String],
) -> Result<Vec<String>, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT opaque_key FROM storage_objects
         WHERE location_id = $1 AND opaque_key = ANY($2) AND durability_state <> 'DELETED'",
    )
    .bind(FS_LOCATION_ID)
    .bind(keys)
    .fetch_all(db)
    .await
}
