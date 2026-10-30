//! Verified emails of actors (`actor_emails`). Personal data: callers never log or audit them.
use sqlx::{PgConnection, PgExecutor};

/// Records `email_norm` as the verified email of `actor_id`. An address verified anew by another
/// account moves to it: PI says who holds it now.
pub async fn upsert_verified(
    conn: &mut PgConnection,
    actor_id: &str,
    email_norm: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM actor_emails WHERE email_norm = $2 AND actor_id <> $1")
        .bind(actor_id)
        .bind(email_norm)
        .execute(&mut *conn)
        .await?;
    sqlx::query(
        "INSERT INTO actor_emails (actor_id, email_norm, verified_at) VALUES ($1, $2, now())
         ON CONFLICT (actor_id) DO UPDATE SET
             email_norm = EXCLUDED.email_norm, verified_at = now(), updated_at = now()",
    )
    .bind(actor_id)
    .bind(email_norm)
    .execute(conn)
    .await
    .map(drop)
}

/// The actor whose verified email is `email_norm`, verified again by PI within `max_age_secs`.
pub async fn actor_of(
    db: impl PgExecutor<'_>,
    email_norm: &str,
    max_age_secs: i64,
) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT actor_id FROM actor_emails
         WHERE email_norm = $1 AND verified_at > now() - make_interval(secs => $2)",
    )
    .bind(email_norm)
    .bind(max_age_secs as f64)
    .fetch_optional(db)
    .await
}

/// The verified email of `actor_id`.
pub async fn email_of(
    db: impl PgExecutor<'_>,
    actor_id: &str,
) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar("SELECT email_norm FROM actor_emails WHERE actor_id = $1")
        .bind(actor_id)
        .fetch_optional(db)
        .await
}

/// Drops the recorded email of `actor_id` (PI no longer reports it verified). Permissions it already
/// took stay: they belong to the account now, not to the address.
pub async fn forget(conn: &mut PgConnection, actor_id: &str) -> Result<u64, sqlx::Error> {
    sqlx::query("DELETE FROM actor_emails WHERE actor_id = $1")
        .bind(actor_id)
        .execute(conn)
        .await
        .map(|r| r.rows_affected())
}
