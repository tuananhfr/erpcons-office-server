//! Transaction-scoped advisory locks. Order, when a transaction takes several: the actor row, then
//! the owner's tree, then an object, then document rows, then a sharer's recipients, then the mail
//! caps. Every writer follows it, so none deadlock. The sign-up lock is never taken with another.
use sqlx::PgConnection;

/// Serializes changes to the folder structure of one owner (moves, trash, restore, creation).
pub async fn tree(conn: &mut PgConnection, owner_id: &str) -> Result<(), sqlx::Error> {
    advisory(conn, &format!("tree:{owner_id}")).await
}

/// Serializes the use of one stored object between saves and the jobs that delete objects.
pub async fn object(conn: &mut PgConnection, sha256: &str) -> Result<(), sqlx::Error> {
    advisory(conn, &format!("object:{sha256}")).await
}

/// Serializes the count of new recipients of one sharer, so parallel requests cannot pass its cap.
pub async fn recipients(conn: &mut PgConnection, actor_id: &str) -> Result<(), sqlx::Error> {
    advisory(conn, &format!("recipients:{actor_id}")).await
}

/// Serializes the mail caps (per sender, per recipient, overall); queuing mail is rare enough to
/// take one lock for all of them.
pub async fn mail_caps(conn: &mut PgConnection) -> Result<(), sqlx::Error> {
    advisory(conn, "mail-caps").await
}

/// Serializes the sign-up caps (per address, whole server), so parallel sign-ups cannot pass them.
pub async fn signup_caps(conn: &mut PgConnection) -> Result<(), sqlx::Error> {
    advisory(conn, "signup-caps").await
}

async fn advisory(conn: &mut PgConnection, key: &str) -> Result<(), sqlx::Error> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(key)
        .execute(conn)
        .await
        .map(drop)
}
