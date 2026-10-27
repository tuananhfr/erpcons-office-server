use sqlx::PgExecutor;

/// Records that `actor_id` was seen now, creating it on first sight. Returns `true` when created.
pub async fn upsert_seen(
    db: impl PgExecutor<'_>,
    actor_id: &str,
    display_name: &str,
) -> Result<bool, sqlx::Error> {
    // `xmax = 0` holds only for a row this statement inserted, not for one it updated.
    sqlx::query_scalar(
        "INSERT INTO actors (id, display_name) VALUES ($1, $2)
         ON CONFLICT (id) DO UPDATE
             SET display_name = EXCLUDED.display_name, last_seen_at = now()
         RETURNING (xmax = 0)",
    )
    .bind(actor_id)
    .bind(display_name)
    .fetch_one(db)
    .await
}
