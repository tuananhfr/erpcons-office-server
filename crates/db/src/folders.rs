use sqlx::{PgConnection, PgExecutor};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::ItemState;

/// The unique index on active folder names (migration 0002).
pub const NAME_INDEX: &str = "folders_active_name_key_ci";

const COLUMNS: &str = "id, owner_id, parent_id, name, state, trashed_at, created_at, updated_at";

/// No `Debug`: the name is personal data and must not reach logs.
#[derive(Clone, sqlx::FromRow)]
pub struct FolderRow {
    pub id: Uuid,
    pub owner_id: String,
    pub parent_id: Option<Uuid>,
    pub name: String,
    #[sqlx(try_from = "String")]
    pub state: ItemState,
    pub trashed_at: Option<OffsetDateTime>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

pub struct NewFolder<'a> {
    pub id: Uuid,
    pub owner_id: &'a str,
    pub parent_id: Option<Uuid>,
    pub name: &'a str,
    pub name_key: &'a str,
}

pub async fn insert(
    db: impl PgExecutor<'_>,
    folder: &NewFolder<'_>,
) -> Result<FolderRow, sqlx::Error> {
    sqlx::query_as(&format!(
        "INSERT INTO folders (id, owner_id, parent_id, name, name_key) VALUES ($1, $2, $3, $4, $5)
         RETURNING {COLUMNS}"
    ))
    .bind(folder.id)
    .bind(folder.owner_id)
    .bind(folder.parent_id)
    .bind(folder.name)
    .bind(folder.name_key)
    .fetch_one(db)
    .await
}

/// Any folder by id, whoever owns it; the caller decides access.
pub async fn get(db: impl PgExecutor<'_>, id: Uuid) -> Result<Option<FolderRow>, sqlx::Error> {
    sqlx::query_as(&format!("SELECT {COLUMNS} FROM folders WHERE id = $1"))
        .bind(id)
        .fetch_optional(db)
        .await
}

/// Sort key of a folder, for resuming a listing after it.
pub async fn sort_key(
    db: impl PgExecutor<'_>,
    owner_id: &str,
    id: Uuid,
) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar("SELECT name_key FROM folders WHERE id = $1 AND owner_id = $2")
        .bind(id)
        .bind(owner_id)
        .fetch_optional(db)
        .await
}

/// Active folders directly in `parent` (`None`: the root), by name then id, after `after`.
pub async fn list_children(
    db: impl PgExecutor<'_>,
    owner_id: &str,
    parent: Option<Uuid>,
    after: Option<(&str, Uuid)>,
    limit: i64,
) -> Result<Vec<FolderRow>, sqlx::Error> {
    sqlx::query_as(&format!(
        "SELECT {COLUMNS} FROM folders
         WHERE owner_id = $1 AND parent_id IS NOT DISTINCT FROM $2 AND state = 'ACTIVE'
           AND ($3::text IS NULL OR (name_key, id) > ($3::text, $4::uuid))
         ORDER BY name_key, id
         LIMIT $5"
    ))
    .bind(owner_id)
    .bind(parent)
    .bind(after.map(|a| a.0))
    .bind(after.map(|a| a.1))
    .bind(limit)
    .fetch_all(db)
    .await
}

pub struct FolderChange<'a> {
    pub name: Option<(&'a str, &'a str)>,
    /// `Some(None)` moves the folder to the root.
    pub parent_id: Option<Option<Uuid>>,
}

pub async fn update(
    db: impl PgExecutor<'_>,
    id: Uuid,
    change: &FolderChange<'_>,
) -> Result<Option<FolderRow>, sqlx::Error> {
    sqlx::query_as(&format!(
        "UPDATE folders SET
             name = COALESCE($2, name),
             name_key = COALESCE($3, name_key),
             parent_id = CASE WHEN $4 THEN $5 ELSE parent_id END,
             updated_at = now()
         WHERE id = $1 AND state = 'ACTIVE'
         RETURNING {COLUMNS}"
    ))
    .bind(id)
    .bind(change.name.map(|n| n.0))
    .bind(change.name.map(|n| n.1))
    .bind(change.parent_id.is_some())
    .bind(change.parent_id.flatten())
    .fetch_optional(db)
    .await
}

/// Whether `folder` is `candidate` or one of its ancestors, i.e. whether moving `folder` into
/// `candidate` would make a cycle.
pub async fn is_ancestor_or_self(
    db: impl PgExecutor<'_>,
    folder: Uuid,
    candidate: Uuid,
) -> Result<bool, sqlx::Error> {
    // UNION, not UNION ALL: the walk ends even if the data ever held a cycle.
    sqlx::query_scalar(
        "WITH RECURSIVE up AS (
             SELECT id, parent_id FROM folders WHERE id = $1
             UNION
             SELECT f.id, f.parent_id FROM folders f JOIN up ON f.id = up.parent_id
         )
         SELECT EXISTS (SELECT 1 FROM up WHERE id = $2)",
    )
    .bind(candidate)
    .bind(folder)
    .fetch_one(db)
    .await
}

/// Trashes an active folder, its active subfolders and the active documents in them, all with
/// the same `trashed_at`: that shared instant is what a restore brings back together.
pub async fn trash_tree(
    conn: &mut PgConnection,
    owner_id: &str,
    id: Uuid,
) -> Result<bool, sqlx::Error> {
    let trashed: Vec<Uuid> = sqlx::query_scalar(
        "WITH RECURSIVE tree AS (
             SELECT id FROM folders WHERE id = $1 AND owner_id = $2 AND state = 'ACTIVE'
             UNION
             SELECT f.id FROM folders f JOIN tree t ON f.parent_id = t.id WHERE f.state = 'ACTIVE'
         )
         UPDATE folders SET state = 'TRASHED', trashed_at = now(), updated_at = now()
         WHERE id IN (SELECT id FROM tree)
         RETURNING id",
    )
    .bind(id)
    .bind(owner_id)
    .fetch_all(&mut *conn)
    .await?;
    if trashed.is_empty() {
        return Ok(false);
    }
    sqlx::query(
        "UPDATE documents SET state = 'TRASHED', trashed_at = now(), updated_at = now()
         WHERE owner_id = $1 AND state = 'ACTIVE' AND folder_id = ANY($2)",
    )
    .bind(owner_id)
    .bind(&trashed)
    .execute(conn)
    .await?;
    Ok(true)
}

/// Restores a trashed folder into `parent_id`, with the subfolders and documents trashed together
/// with it (same `trashed_at`). Items trashed on their own before stay in the trash.
pub async fn restore_tree(
    conn: &mut PgConnection,
    owner_id: &str,
    id: Uuid,
    parent_id: Option<Uuid>,
) -> Result<Option<FolderRow>, sqlx::Error> {
    let batch: Vec<Uuid> = sqlx::query_scalar(
        "WITH RECURSIVE batch AS (
             SELECT id, trashed_at FROM folders WHERE id = $1 AND owner_id = $2 AND state = 'TRASHED'
             UNION
             SELECT f.id, f.trashed_at FROM folders f JOIN batch b ON f.parent_id = b.id
             WHERE f.state = 'TRASHED' AND f.trashed_at = b.trashed_at
         )
         SELECT id FROM batch",
    )
    .bind(id)
    .bind(owner_id)
    .fetch_all(&mut *conn)
    .await?;
    if batch.is_empty() {
        return Ok(None);
    }
    sqlx::query(
        "UPDATE documents d SET state = 'ACTIVE', trashed_at = NULL, updated_at = now()
         FROM folders f
         WHERE f.id = ANY($2) AND d.owner_id = $1 AND d.folder_id = f.id
           AND d.state = 'TRASHED' AND d.trashed_at = f.trashed_at",
    )
    .bind(owner_id)
    .bind(&batch)
    .execute(&mut *conn)
    .await?;
    // The top folder last: its new parent is the only place a name can collide.
    sqlx::query(
        "UPDATE folders SET state = 'ACTIVE', trashed_at = NULL, updated_at = now()
         WHERE id = ANY($1) AND id <> $2",
    )
    .bind(&batch)
    .bind(id)
    .execute(&mut *conn)
    .await?;
    sqlx::query_as(&format!(
        "UPDATE folders SET state = 'ACTIVE', trashed_at = NULL, parent_id = $2, updated_at = now()
         WHERE id = $1
         RETURNING {COLUMNS}"
    ))
    .bind(id)
    .bind(parent_id)
    .fetch_optional(conn)
    .await
}

/// Top-level trashed folders (not trashed as part of a trashed parent), newest first, after
/// `(trashed_at, rank, id)` in the trash order where folders rank 0 and documents rank 1.
pub async fn list_trash(
    db: impl PgExecutor<'_>,
    owner_id: &str,
    after: Option<(OffsetDateTime, i16, Uuid)>,
    limit: i64,
) -> Result<Vec<FolderRow>, sqlx::Error> {
    sqlx::query_as(&format!(
        "SELECT {COLUMNS} FROM folders f
         WHERE f.owner_id = $1 AND f.state = 'TRASHED'
           AND NOT EXISTS (
               SELECT 1 FROM folders p
               WHERE p.id = f.parent_id AND p.state = 'TRASHED' AND p.trashed_at = f.trashed_at)
           AND ($2::timestamptz IS NULL OR f.trashed_at < $2
                OR (f.trashed_at = $2 AND ($3 < 0 OR ($3 = 0 AND f.id > $4))))
         ORDER BY f.trashed_at DESC, f.id
         LIMIT $5"
    ))
    .bind(owner_id)
    .bind(after.map(|a| a.0))
    .bind(after.map_or(0, |a| a.1))
    .bind(after.map(|a| a.2))
    .bind(limit)
    .fetch_all(db)
    .await
}
