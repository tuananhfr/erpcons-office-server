//! Mail waiting to be sent (`mail_outbox`). Rows hold addresses and names: never logged.
use sqlx::{PgConnection, PgExecutor};
use time::OffsetDateTime;
use uuid::Uuid;

/// One mail to queue; `payload` is JSON.
pub struct NewMail<'a> {
    pub kind: &'a str,
    /// The actor whose action causes the mail; counted against its daily cap.
    pub sender_id: Option<&'a str>,
    pub to_email: &'a str,
    pub payload: &'a str,
}

/// Queues one mail inside the caller's transaction, so it exists only if that commits.
pub async fn enqueue(conn: &mut PgConnection, mail: &NewMail<'_>) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO mail_outbox (id, kind, sender_id, to_email, payload)
         VALUES ($1, $2, $3, $4, $5::jsonb)",
    )
    .bind(Uuid::now_v7())
    .bind(mail.kind)
    .bind(mail.sender_id)
    .bind(mail.to_email)
    .bind(mail.payload)
    .execute(conn)
    .await
    .map(drop)
}

/// Mails `sender_id` caused in the last `secs` seconds, sent or not.
pub async fn caused_since(
    db: impl PgExecutor<'_>,
    sender_id: &str,
    secs: i64,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT count(*) FROM mail_outbox
         WHERE sender_id = $1 AND created_at > now() - make_interval(secs => $2)",
    )
    .bind(sender_id)
    .bind(secs as f64)
    .fetch_one(db)
    .await
}

/// Mails to `to_email` in the last `secs` seconds, from anyone.
pub async fn received_since(
    db: impl PgExecutor<'_>,
    to_email: &str,
    secs: i64,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT count(*) FROM mail_outbox
         WHERE to_email = $1 AND created_at > now() - make_interval(secs => $2)",
    )
    .bind(to_email)
    .bind(secs as f64)
    .fetch_one(db)
    .await
}

/// All mails queued in the last `secs` seconds.
pub async fn queued_since(db: impl PgExecutor<'_>, secs: i64) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT count(*) FROM mail_outbox WHERE created_at > now() - make_interval(secs => $1)",
    )
    .bind(secs as f64)
    .fetch_one(db)
    .await
}

/// Mails of `kind` to `to_email` in the last `secs` seconds.
pub async fn received_of_kind_since(
    db: impl PgExecutor<'_>,
    to_email: &str,
    kind: &str,
    secs: i64,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT count(*) FROM mail_outbox
         WHERE to_email = $1 AND kind = $2 AND created_at > now() - make_interval(secs => $3)",
    )
    .bind(to_email)
    .bind(kind)
    .bind(secs as f64)
    .fetch_one(db)
    .await
}

/// Mails of `kind` queued in the last `secs` seconds, to anyone.
pub async fn queued_of_kind_since(
    db: impl PgExecutor<'_>,
    kind: &str,
    secs: i64,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT count(*) FROM mail_outbox
         WHERE kind = $1 AND created_at > now() - make_interval(secs => $2)",
    )
    .bind(kind)
    .bind(secs as f64)
    .fetch_one(db)
    .await
}

/// No `Debug`: holds an address and a name.
#[derive(Clone, sqlx::FromRow)]
pub struct OutboxRow {
    pub id: Uuid,
    pub kind: String,
    pub to_email: String,
    pub payload: String,
    /// Including the attempt this claim starts.
    pub attempts: i32,
    pub created_at: OffsetDateTime,
}

/// Claims up to `limit` mails due by `due_by` for `lease_secs`: a sender that dies mid-send leaves
/// them to be tried again once the lease ends, and two senders never take the same row.
pub async fn claim_due(
    db: impl PgExecutor<'_>,
    due_by: OffsetDateTime,
    limit: i64,
    lease_secs: i64,
) -> Result<Vec<OutboxRow>, sqlx::Error> {
    sqlx::query_as(
        "UPDATE mail_outbox SET attempts = attempts + 1, next_at = now() + make_interval(secs => $2)
         WHERE id IN (
             SELECT id FROM mail_outbox
             WHERE sent_at IS NULL AND failed_at IS NULL AND next_at <= LEAST(now(), $3)
             ORDER BY next_at
             LIMIT $1
             FOR UPDATE SKIP LOCKED)
         RETURNING id, kind, to_email, payload::text AS payload, attempts, created_at",
    )
    .bind(limit)
    .bind(lease_secs as f64)
    .bind(due_by)
    .fetch_all(db)
    .await
}

pub async fn mark_sent(db: impl PgExecutor<'_>, id: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE mail_outbox SET sent_at = now() WHERE id = $1")
        .bind(id)
        .execute(db)
        .await
        .map(drop)
}

pub async fn retry_in(db: impl PgExecutor<'_>, id: Uuid, secs: i64) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE mail_outbox SET next_at = now() + make_interval(secs => $2) WHERE id = $1")
        .bind(id)
        .bind(secs as f64)
        .execute(db)
        .await
        .map(drop)
}

pub async fn mark_failed(db: impl PgExecutor<'_>, id: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE mail_outbox SET failed_at = now() WHERE id = $1")
        .bind(id)
        .execute(db)
        .await
        .map(drop)
}

/// Deletes sent or given-up mails older than `secs`, so addresses and messages do not pile up.
pub async fn delete_done_older_than(
    db: impl PgExecutor<'_>,
    secs: i64,
) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(
        "DELETE FROM mail_outbox
         WHERE (sent_at IS NOT NULL OR failed_at IS NOT NULL)
           AND created_at < now() - make_interval(secs => $1)",
    )
    .bind(secs as f64)
    .execute(db)
    .await?;
    Ok(result.rows_affected())
}
