//! Self sign-ups (`signups`). Rows hold an address, a name and a password hash: never logged.
use sqlx::{PgConnection, PgExecutor};
use time::OffsetDateTime;
use uuid::Uuid;

/// A sign-up as the request submits it. No `Debug`: personal data and a password hash.
pub struct NewSignup<'a> {
    pub id: Uuid,
    pub email: &'a str,
    pub display_name: &'a str,
    pub password_hash: &'a str,
}

/// Records a pending sign-up, or replaces the name and hash of the address's pending one (and voids
/// its token, so only the next mail's link works). `None` when the address is already verified and
/// waiting for PI: that sign-up is left alone.
pub async fn submit(
    conn: &mut PgConnection,
    signup: &NewSignup<'_>,
) -> Result<Option<Uuid>, sqlx::Error> {
    sqlx::query_scalar(
        "INSERT INTO signups (id, email, display_name, password_hash, state)
         VALUES ($1, $2, $3, $4, 'pending')
         ON CONFLICT (email) WHERE state IN ('pending', 'verified') DO UPDATE
             SET display_name = EXCLUDED.display_name, password_hash = EXCLUDED.password_hash,
                 submitted_at = now(), token_sha256 = NULL, token_expires_at = NULL
             WHERE signups.state = 'pending'
         RETURNING id",
    )
    .bind(signup.id)
    .bind(signup.email)
    .bind(signup.display_name)
    .bind(signup.password_hash)
    .fetch_optional(conn)
    .await
}

/// Stores the hash of a new token for a pending sign-up, replacing any earlier one; returns the
/// name to greet, or `None` when the sign-up is gone or no longer pending.
pub async fn issue_token(
    db: impl PgExecutor<'_>,
    id: Uuid,
    token_sha256: &str,
    ttl_secs: i64,
) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar(
        "UPDATE signups SET token_sha256 = $2, token_expires_at = now() + make_interval(secs => $3)
         WHERE id = $1 AND state = 'pending'
         RETURNING display_name",
    )
    .bind(id)
    .bind(token_sha256)
    .bind(ttl_secs as f64)
    .fetch_optional(db)
    .await
}

/// Uses a token once: its pending, unexpired sign-up becomes verified and due for PI at once.
pub async fn verify(
    db: impl PgExecutor<'_>,
    token_sha256: &str,
) -> Result<Option<Uuid>, sqlx::Error> {
    sqlx::query_scalar(
        "UPDATE signups
         SET state = 'verified', token_sha256 = NULL, token_expires_at = NULL, verified_at = now(),
             push_next_at = now()
         WHERE token_sha256 = $1 AND state = 'pending' AND token_expires_at > now()
         RETURNING id",
    )
    .bind(token_sha256)
    .fetch_optional(db)
    .await
}

/// A verified sign-up claimed for one attempt at PI. No `Debug`: personal data and a hash.
#[derive(Clone, sqlx::FromRow)]
pub struct PushRow {
    pub id: Uuid,
    pub email: String,
    pub display_name: String,
    pub password_hash: String,
    /// Including the attempt this claim starts.
    pub push_attempts: i32,
    pub verified_at: OffsetDateTime,
}

/// Claims up to `limit` verified sign-ups due by `due_by` for `lease_secs`, as the mail outbox does:
/// a job that dies mid-call leaves them to be tried again once the lease ends.
pub async fn claim_push(
    db: impl PgExecutor<'_>,
    due_by: OffsetDateTime,
    limit: i64,
    lease_secs: i64,
) -> Result<Vec<PushRow>, sqlx::Error> {
    sqlx::query_as(
        "UPDATE signups
         SET push_attempts = push_attempts + 1, push_next_at = now() + make_interval(secs => $2)
         WHERE id IN (
             SELECT id FROM signups
             WHERE state = 'verified' AND push_next_at <= LEAST(now(), $3)
             ORDER BY push_next_at
             LIMIT $1
             FOR UPDATE SKIP LOCKED)
         RETURNING id, email, display_name, password_hash, push_attempts, verified_at",
    )
    .bind(limit)
    .bind(lease_secs as f64)
    .bind(due_by)
    .fetch_all(db)
    .await
}

pub async fn retry_push_in(
    db: impl PgExecutor<'_>,
    id: Uuid,
    secs: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE signups SET push_next_at = now() + make_interval(secs => $2)
         WHERE id = $1 AND state = 'verified'",
    )
    .bind(id)
    .bind(secs as f64)
    .execute(db)
    .await
    .map(drop)
}

/// Ends a verified sign-up (`registered` or `failed`) and drops its password hash.
pub async fn finish(
    db: impl PgExecutor<'_>,
    id: Uuid,
    registered: bool,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query(
        "UPDATE signups
         SET state = $2, password_hash = NULL, push_next_at = NULL, finished_at = now()
         WHERE id = $1 AND state = 'verified'",
    )
    .bind(id)
    .bind(if registered { "registered" } else { "failed" })
    .execute(db)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// Deletes sign-ups never verified within `pending_secs` of their last submission, and finished
/// ones `finished_secs` after they ended. Returns (pending, finished) counts.
pub async fn expire(
    db: impl PgExecutor<'_> + Copy,
    pending_secs: i64,
    finished_secs: i64,
) -> Result<(u64, u64), sqlx::Error> {
    let pending = sqlx::query(
        "DELETE FROM signups
         WHERE state = 'pending' AND submitted_at < now() - make_interval(secs => $1)",
    )
    .bind(pending_secs as f64)
    .execute(db)
    .await?
    .rows_affected();
    let finished = sqlx::query(
        "DELETE FROM signups
         WHERE state IN ('registered', 'failed') AND finished_at < now() - make_interval(secs => $1)",
    )
    .bind(finished_secs as f64)
    .execute(db)
    .await?
    .rows_affected();
    Ok((pending, finished))
}
