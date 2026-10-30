use sqlx::PgExecutor;

/// Records that `actor_id` was seen now, creating it on first sight. Returns `true` when created.
/// An empty `display_name` (PI could not be asked) keeps the stored one.
pub async fn upsert_seen(
    db: impl PgExecutor<'_>,
    actor_id: &str,
    display_name: &str,
) -> Result<bool, sqlx::Error> {
    // `xmax = 0` holds only for a row this statement inserted, not for one it updated.
    sqlx::query_scalar(
        "INSERT INTO actors (id, display_name) VALUES ($1, $2)
         ON CONFLICT (id) DO UPDATE
             SET display_name = COALESCE(NULLIF(EXCLUDED.display_name, ''), actors.display_name),
                 last_seen_at = now()
         RETURNING (xmax = 0)",
    )
    .bind(actor_id)
    .bind(display_name)
    .fetch_one(db)
    .await
}

/// Locks the row of `actor_id` until the transaction ends, serializing the quota checks of that owner.
/// `NO KEY UPDATE`, not `UPDATE`: the foreign-key checks of rows naming this actor (`FOR KEY SHARE`)
/// must not wait for it, or a share holding a document row and a save holding the owner deadlock.
pub async fn lock(conn: &mut sqlx::PgConnection, actor_id: &str) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar("SELECT true FROM actors WHERE id = $1 FOR NO KEY UPDATE")
        .bind(actor_id)
        .fetch_optional(conn)
        .await
        .map(|row: Option<bool>| row.is_some())
}
