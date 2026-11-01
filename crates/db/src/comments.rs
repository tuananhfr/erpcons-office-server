//! Comment threads and their comments (`comment_threads`, `comments`). Bodies, anchors and names are
//! personal data: never logged. Every query that takes a thread id also takes its document id, so
//! a thread is only ever reached through a document the caller was let into.
use sqlx::{PgConnection, PgExecutor};
use time::OffsetDateTime;
use uuid::Uuid;

/// No `Debug`: holds names and the anchor.
#[derive(Clone, sqlx::FromRow)]
pub struct ThreadRow {
    pub id: Uuid,
    pub document_id: Uuid,
    pub anchor: String,
    pub region: Option<String>,
    pub created_version_seq: i32,
    pub status: String,
    pub created_by: String,
    pub created_by_name: String,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
    pub resolved_by: Option<String>,
    pub resolved_by_name: Option<String>,
    pub resolved_at: Option<OffsetDateTime>,
}

/// No `Debug`: holds the body and a name.
#[derive(Clone, sqlx::FromRow)]
pub struct CommentRow {
    pub id: Uuid,
    pub thread_id: Uuid,
    pub author_id: String,
    pub author_name: String,
    pub body: Option<String>,
    pub created_at: OffsetDateTime,
    pub edited_at: Option<OffsetDateTime>,
    pub deleted_at: Option<OffsetDateTime>,
}

const THREAD: &str = "SELECT t.id, t.document_id, t.anchor::text AS anchor, t.region,
         t.created_version_seq, t.status, t.created_by, c.display_name AS created_by_name,
         t.created_at, t.updated_at, t.resolved_by, r.display_name AS resolved_by_name,
         t.resolved_at
     FROM comment_threads t
     JOIN actors c ON c.id = t.created_by
     LEFT JOIN actors r ON r.id = t.resolved_by";

const COMMENT: &str = "SELECT m.id, m.thread_id, m.author_id, a.display_name AS author_name,
         m.body, m.created_at, m.edited_at, m.deleted_at
     FROM comments m JOIN actors a ON a.id = m.author_id";

pub struct NewThread<'a> {
    pub id: Uuid,
    pub document_id: Uuid,
    /// A JSON object.
    pub anchor: &'a str,
    pub region: Option<&'a str>,
    pub created_version_seq: i32,
    pub created_by: &'a str,
}

pub async fn insert_thread(
    conn: &mut PgConnection,
    thread: &NewThread<'_>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO comment_threads (id, document_id, anchor, region, created_version_seq, created_by)
         VALUES ($1, $2, $3::jsonb, $4, $5, $6)",
    )
    .bind(thread.id)
    .bind(thread.document_id)
    .bind(thread.anchor)
    .bind(thread.region)
    .bind(thread.created_version_seq)
    .bind(thread.created_by)
    .execute(conn)
    .await
    .map(drop)
}

pub async fn insert_comment(
    conn: &mut PgConnection,
    id: Uuid,
    thread_id: Uuid,
    author_id: &str,
    body: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT INTO comments (id, thread_id, author_id, body) VALUES ($1, $2, $3, $4)")
        .bind(id)
        .bind(thread_id)
        .bind(author_id)
        .bind(body)
        .execute(&mut *conn)
        .await?;
    touch(conn, thread_id).await
}

/// The thread's `updated_at`: any change to it or its comments.
async fn touch(conn: &mut PgConnection, thread_id: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE comment_threads SET updated_at = now() WHERE id = $1")
        .bind(thread_id)
        .execute(conn)
        .await
        .map(drop)
}

/// Threads of `document_id` after `after` (by id, which follows creation), at most `limit`; only
/// those in `status` when given.
pub async fn list_threads(
    db: impl PgExecutor<'_>,
    document_id: Uuid,
    status: Option<&str>,
    after: Option<Uuid>,
    limit: i64,
) -> Result<Vec<ThreadRow>, sqlx::Error> {
    sqlx::query_as(&format!(
        "{THREAD} WHERE t.document_id = $1 AND ($2::text IS NULL OR t.status = $2)
             AND ($3::uuid IS NULL OR t.id > $3)
         ORDER BY t.id LIMIT $4"
    ))
    .bind(document_id)
    .bind(status)
    .bind(after)
    .bind(limit)
    .fetch_all(db)
    .await
}

pub async fn thread(
    db: impl PgExecutor<'_>,
    document_id: Uuid,
    thread_id: Uuid,
) -> Result<Option<ThreadRow>, sqlx::Error> {
    sqlx::query_as(&format!("{THREAD} WHERE t.document_id = $1 AND t.id = $2"))
        .bind(document_id)
        .bind(thread_id)
        .fetch_optional(db)
        .await
}

/// Locks the thread row until the transaction ends; `false` when it is not one of `document_id`.
pub async fn lock_thread(
    conn: &mut PgConnection,
    document_id: Uuid,
    thread_id: Uuid,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar::<_, Uuid>(
        "SELECT id FROM comment_threads WHERE document_id = $1 AND id = $2 FOR UPDATE",
    )
    .bind(document_id)
    .bind(thread_id)
    .fetch_optional(conn)
    .await
    .map(|row| row.is_some())
}

/// Comments of `thread_ids`, by thread then creation.
pub async fn comments_of(
    db: impl PgExecutor<'_>,
    thread_ids: &[Uuid],
) -> Result<Vec<CommentRow>, sqlx::Error> {
    sqlx::query_as(&format!(
        "{COMMENT} WHERE m.thread_id = ANY($1) ORDER BY m.thread_id, m.id"
    ))
    .bind(thread_ids)
    .fetch_all(db)
    .await
}

pub async fn comment(
    db: impl PgExecutor<'_>,
    thread_id: Uuid,
    comment_id: Uuid,
) -> Result<Option<CommentRow>, sqlx::Error> {
    sqlx::query_as(&format!("{COMMENT} WHERE m.thread_id = $1 AND m.id = $2"))
        .bind(thread_id)
        .bind(comment_id)
        .fetch_optional(db)
        .await
}

pub async fn count_threads(db: impl PgExecutor<'_>, document_id: Uuid) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar("SELECT count(*) FROM comment_threads WHERE document_id = $1")
        .bind(document_id)
        .fetch_one(db)
        .await
}

pub async fn count_comments(db: impl PgExecutor<'_>, thread_id: Uuid) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar("SELECT count(*) FROM comments WHERE thread_id = $1")
        .bind(thread_id)
        .fetch_one(db)
        .await
}

/// Replaces the body of a comment not deleted; `false` when there is none.
pub async fn edit(
    conn: &mut PgConnection,
    thread_id: Uuid,
    comment_id: Uuid,
    body: &str,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query(
        "UPDATE comments SET body = $3, edited_at = now()
         WHERE thread_id = $1 AND id = $2 AND deleted_at IS NULL",
    )
    .bind(thread_id)
    .bind(comment_id)
    .bind(body)
    .execute(&mut *conn)
    .await?;
    if result.rows_affected() == 0 {
        return Ok(false);
    }
    touch(conn, thread_id).await.map(|()| true)
}

/// Deletes the words of a comment, keeping its place; `false` when already deleted or missing.
pub async fn soft_delete(
    conn: &mut PgConnection,
    thread_id: Uuid,
    comment_id: Uuid,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query(
        "UPDATE comments SET body = NULL, deleted_at = now()
         WHERE thread_id = $1 AND id = $2 AND deleted_at IS NULL",
    )
    .bind(thread_id)
    .bind(comment_id)
    .execute(&mut *conn)
    .await?;
    if result.rows_affected() == 0 {
        return Ok(false);
    }
    touch(conn, thread_id).await.map(|()| true)
}

/// Comments of the thread still holding words.
pub async fn live_comments(db: impl PgExecutor<'_>, thread_id: Uuid) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar("SELECT count(*) FROM comments WHERE thread_id = $1 AND deleted_at IS NULL")
        .bind(thread_id)
        .fetch_one(db)
        .await
}

/// Deletes the thread with its comments; `false` when it is not one of `document_id`.
pub async fn delete_thread(
    conn: &mut PgConnection,
    document_id: Uuid,
    thread_id: Uuid,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query("DELETE FROM comment_threads WHERE document_id = $1 AND id = $2")
        .bind(document_id)
        .bind(thread_id)
        .execute(conn)
        .await?;
    Ok(result.rows_affected() == 1)
}

/// Marks the thread resolved by `actor_id`; one already resolved keeps who resolved it first.
pub async fn resolve(
    conn: &mut PgConnection,
    document_id: Uuid,
    thread_id: Uuid,
    actor_id: &str,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query(
        "UPDATE comment_threads
         SET status = 'resolved', resolved_by = $3, resolved_at = now(), updated_at = now()
         WHERE document_id = $1 AND id = $2 AND status <> 'resolved'",
    )
    .bind(document_id)
    .bind(thread_id)
    .bind(actor_id)
    .execute(conn)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// A resolved thread is open again; until its editor re-places it, it counts as placed.
pub async fn reopen(
    conn: &mut PgConnection,
    document_id: Uuid,
    thread_id: Uuid,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query(
        "UPDATE comment_threads
         SET status = 'open', resolved_by = NULL, resolved_at = NULL, updated_at = now()
         WHERE document_id = $1 AND id = $2 AND status = 'resolved'",
    )
    .bind(document_id)
    .bind(thread_id)
    .execute(conn)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// What an editor found when it placed a thread again; `None` fields stay as they are.
pub struct AnchorChange<'a> {
    pub thread_id: Uuid,
    pub anchor: Option<&'a str>,
    /// `Some(None)` clears the region.
    pub region: Option<Option<&'a str>>,
    /// `open` or `orphaned`; a resolved thread stays resolved.
    pub status: Option<&'a str>,
}

/// Applies one change to a thread of `document_id`; `false` when there is no such thread.
pub async fn change_anchor(
    conn: &mut PgConnection,
    document_id: Uuid,
    change: &AnchorChange<'_>,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query(
        "UPDATE comment_threads SET
             anchor = COALESCE($3::jsonb, anchor),
             region = CASE WHEN $4 THEN $5 ELSE region END,
             status = CASE WHEN $6::text IS NULL OR status = 'resolved' THEN status ELSE $6 END,
             updated_at = now()
         WHERE document_id = $1 AND id = $2",
    )
    .bind(document_id)
    .bind(change.thread_id)
    .bind(change.anchor)
    .bind(change.region.is_some())
    .bind(change.region.flatten())
    .bind(change.status)
    .execute(conn)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// Someone who wrote in a thread, with what decides whether a mail may reach them.
#[derive(Clone, sqlx::FromRow)]
pub struct Participant {
    pub actor_id: String,
    /// Their verified address, if PI gave one.
    pub email: Option<String>,
    /// The role of a permission in force on the thread's document; `None` for the owner and for
    /// people who lost access.
    pub grant_role: Option<String>,
}

/// Authors of words still in the thread, `except` excluded.
pub async fn participants(
    db: impl PgExecutor<'_>,
    thread_id: Uuid,
    except: &str,
) -> Result<Vec<Participant>, sqlx::Error> {
    sqlx::query_as(
        "SELECT p.author_id AS actor_id, e.email_norm AS email, g.role AS grant_role
         FROM (SELECT DISTINCT m.author_id, t.document_id FROM comments m
               JOIN comment_threads t ON t.id = m.thread_id
               WHERE m.thread_id = $1 AND m.deleted_at IS NULL AND m.author_id <> $2) p
         LEFT JOIN actor_emails e ON e.actor_id = p.author_id
         LEFT JOIN LATERAL (
             SELECT role FROM document_acl
             WHERE document_id = p.document_id AND actor_id = p.author_id AND revoked_at IS NULL
               AND (expires_at IS NULL OR expires_at > now())) g ON true
         ORDER BY p.author_id",
    )
    .bind(thread_id)
    .bind(except)
    .fetch_all(db)
    .await
}

/// Threads of `document_id` started at or after `since` by anyone but `owner_id`.
pub async fn new_threads_since(
    db: impl PgExecutor<'_>,
    document_id: Uuid,
    owner_id: &str,
    since: OffsetDateTime,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT count(*) FROM comment_threads
         WHERE document_id = $1 AND created_by <> $2 AND created_at >= $3",
    )
    .bind(document_id)
    .bind(owner_id)
    .bind(since)
    .fetch_one(db)
    .await
}
