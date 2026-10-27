use sqlx::{PgExecutor, PgPool};

/// The state of `(actor, key)` after trying to claim it.
pub enum Claim {
    /// The key is ours: run the request, then [`complete`] or [`release`] it.
    New,
    /// Seen within its lifetime: `response` is `None` while the first request still runs.
    Existing {
        request_hash: String,
        response: Option<String>,
    },
}

/// Claims `key` for a request hashing to `request_hash`. An expired key, or a claim left
/// unfinished for `stale_secs` (a crash mid-request), is taken over.
pub async fn claim(
    pool: &PgPool,
    actor_id: &str,
    key: &str,
    request_hash: &str,
    ttl_secs: i64,
    stale_secs: i64,
) -> Result<Claim, sqlx::Error> {
    let claimed: Option<bool> = sqlx::query_scalar(
        "INSERT INTO idempotency_keys (actor_id, key, request_hash, expires_at)
         VALUES ($1, $2, $3, now() + make_interval(secs => $4))
         ON CONFLICT (actor_id, key) DO UPDATE SET
             request_hash = EXCLUDED.request_hash, response = NULL,
             created_at = now(), expires_at = EXCLUDED.expires_at
         WHERE idempotency_keys.expires_at <= now()
            OR (idempotency_keys.response IS NULL
                AND idempotency_keys.created_at < now() - make_interval(secs => $5))
         RETURNING true",
    )
    .bind(actor_id)
    .bind(key)
    .bind(request_hash)
    .bind(ttl_secs as f64)
    .bind(stale_secs as f64)
    .fetch_optional(pool)
    .await?;
    if claimed.is_some() {
        return Ok(Claim::New);
    }
    let (request_hash, response): (String, Option<String>) = sqlx::query_as(
        "SELECT request_hash, response::text FROM idempotency_keys WHERE actor_id = $1 AND key = $2",
    )
    .bind(actor_id)
    .bind(key)
    .fetch_one(pool)
    .await?;
    Ok(Claim::Existing {
        request_hash,
        response,
    })
}

/// Stores the response a replay of `key` returns. `response` is JSON.
pub async fn complete(
    db: impl PgExecutor<'_>,
    actor_id: &str,
    key: &str,
    response: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE idempotency_keys SET response = $3::jsonb WHERE actor_id = $1 AND key = $2")
        .bind(actor_id)
        .bind(key)
        .bind(response)
        .execute(db)
        .await
        .map(drop)
}

/// Gives an unfinished claim up, so a retry with the same key runs again.
pub async fn release(
    db: impl PgExecutor<'_>,
    actor_id: &str,
    key: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "DELETE FROM idempotency_keys WHERE actor_id = $1 AND key = $2 AND response IS NULL",
    )
    .bind(actor_id)
    .bind(key)
    .execute(db)
    .await
    .map(drop)
}

pub async fn delete_expired(db: impl PgExecutor<'_>) -> Result<u64, sqlx::Error> {
    let result = sqlx::query("DELETE FROM idempotency_keys WHERE expires_at <= now()")
        .execute(db)
        .await?;
    Ok(result.rows_affected())
}
