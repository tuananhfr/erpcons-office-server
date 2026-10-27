//! Transaction-scoped advisory locks. Order, when a transaction takes several: the actor row, then
//! the owner's tree, then an object, then document rows. Every writer follows it, so none deadlock.
use sqlx::PgConnection;

/// Serializes changes to the folder structure of one owner (moves, trash, restore, creation).
pub async fn tree(conn: &mut PgConnection, owner_id: &str) -> Result<(), sqlx::Error> {
    advisory(conn, &format!("tree:{owner_id}")).await
}

/// Serializes the use of one stored object between saves and the jobs that delete objects.
pub async fn object(conn: &mut PgConnection, sha256: &str) -> Result<(), sqlx::Error> {
    advisory(conn, &format!("object:{sha256}")).await
}

async fn advisory(conn: &mut PgConnection, key: &str) -> Result<(), sqlx::Error> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(key)
        .execute(conn)
        .await
        .map(drop)
}
