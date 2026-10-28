use office_db::{
    PgPool, actors,
    audit::{self, ActorKind, AuditEvent, Outcome},
    quota,
    testing::TestDb,
};
use time::OffsetDateTime;
use uuid::Uuid;

const LOCATION: &str = "00000000-0000-0000-0000-000000000001";
const UNIQUE: &str = "23505";
const FOREIGN_KEY: &str = "23503";
const CHECK: &str = "23514";
const RAISED: &str = "P0001";

async fn actor(pool: &PgPool, id: &str) {
    actors::upsert_seen(pool, id, "Tên").await.expect("actor");
}

async fn folder(
    pool: &PgPool,
    owner: &str,
    parent: Option<Uuid>,
    name: &str,
) -> Result<Uuid, sqlx::Error> {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO folders (id, owner_id, parent_id, name, name_key) VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(id)
    .bind(owner)
    .bind(parent)
    .bind(name)
    .bind(name.to_lowercase())
    .execute(pool)
    .await?;
    Ok(id)
}

async fn object(pool: &PgPool, sha: &str, size: i64) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO storage_objects (id, location_id, opaque_key, sha256, size)
         VALUES ($1, $2::uuid, $3, $3, $4)",
    )
    .bind(id)
    .bind(LOCATION)
    .bind(sha)
    .bind(size)
    .execute(pool)
    .await
    .expect("object");
    id
}

async fn draft(pool: &PgPool, owner: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO documents (id, owner_id, title, kind, mime)
         VALUES ($1, $2, 'Báo cáo', 'word', 'application/octet-stream')",
    )
    .bind(id)
    .bind(owner)
    .execute(pool)
    .await
    .expect("document");
    id
}

struct NewVersion<'a> {
    doc: Uuid,
    seq: i32,
    object: Uuid,
    sha: &'a str,
    bytes: i64,
}

async fn version(pool: &PgPool, v: NewVersion<'_>) -> Result<Uuid, sqlx::Error> {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO document_versions
             (id, document_id, seq, storage_object_id, sha256, bytes, author_id, author_name_snapshot)
         SELECT $1, $2, $3, $4, $5, $6, owner_id, 'Tên' FROM documents WHERE id = $2",
    )
    .bind(id)
    .bind(v.doc)
    .bind(v.seq)
    .bind(v.object)
    .bind(v.sha)
    .bind(v.bytes)
    .execute(pool)
    .await?;
    Ok(id)
}

async fn activate(pool: &PgPool, doc: Uuid, version: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE documents SET state = 'ACTIVE', current_version_id = $2, etag = $2::text
         WHERE id = $1",
    )
    .bind(doc)
    .bind(version)
    .execute(pool)
    .await
    .map(drop)
}

async fn exec(pool: &PgPool, sql: &str, id: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query(sql).bind(id).execute(pool).await.map(drop)
}

fn sha(c: char) -> String {
    c.to_string().repeat(64)
}

fn violates<T>(result: Result<T, sqlx::Error>, sqlstate: &str) {
    match result {
        Err(sqlx::Error::Database(e)) => {
            assert_eq!(e.code().as_deref(), Some(sqlstate), "{e}");
        }
        Err(e) => panic!("unexpected error: {e}"),
        Ok(_) => panic!("expected SQLSTATE {sqlstate}"),
    }
}

#[tokio::test]
async fn migration_seeds_the_filesystem_location() {
    let Some(db) = TestDb::create().await else {
        return;
    };
    let provider: String = sqlx::query_scalar("SELECT provider_type FROM storage_locations")
        .fetch_one(db.pool())
        .await
        .expect("one location");
    assert_eq!(provider, "fs");
    office_db::ping(db.pool()).await.expect("ping");
    office_db::migrate(db.pool())
        .await
        .expect("migrations are idempotent");
}

#[tokio::test]
async fn a_schema_migrated_by_a_newer_release_still_starts() {
    let Some(db) = TestDb::create().await else {
        return;
    };
    sqlx::query(
        "INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time)
         VALUES (99991231000000, 'from a newer release', true, decode('00', 'hex'), 0)",
    )
    .execute(db.pool())
    .await
    .expect("record a migration this build does not know");
    office_db::migrate(db.pool())
        .await
        .expect("a rollback to this build must not refuse the newer schema");
}

#[tokio::test]
async fn upsert_seen_creates_once_then_updates() {
    let Some(db) = TestDb::create().await else {
        return;
    };
    let pool = db.pool();
    assert!(
        actors::upsert_seen(pool, "usr_1", "An")
            .await
            .expect("insert")
    );
    let first: OffsetDateTime = sqlx::query_scalar("SELECT last_seen_at FROM actors")
        .fetch_one(pool)
        .await
        .expect("row");
    let created = actors::upsert_seen(pool, "usr_1", "An Nguyễn")
        .await
        .expect("update");
    assert!(!created);
    let (name, seen): (String, OffsetDateTime) =
        sqlx::query_as("SELECT display_name, last_seen_at FROM actors WHERE id = 'usr_1'")
            .fetch_one(pool)
            .await
            .expect("row");
    assert_eq!(name, "An Nguyễn");
    assert!(seen >= first);

    actors::upsert_seen(pool, "usr_1", "")
        .await
        .expect("update");
    let name: String = sqlx::query_scalar("SELECT display_name FROM actors WHERE id = 'usr_1'")
        .fetch_one(pool)
        .await
        .expect("row");
    assert_eq!(
        name, "An Nguyễn",
        "an unknown name keeps the last known one"
    );
}

#[tokio::test]
async fn folder_names_are_unique_per_parent_among_active_folders() {
    let Some(db) = TestDb::create().await else {
        return;
    };
    let pool = db.pool();
    actor(pool, "a").await;
    actor(pool, "b").await;
    let root = folder(pool, "a", None, "Dự án").await.expect("root folder");
    violates(folder(pool, "a", None, "Dự án").await, UNIQUE);
    violates(folder(pool, "a", None, "DỰ ÁN").await, UNIQUE);
    folder(pool, "b", None, "Dự án")
        .await
        .expect("another owner may reuse the name");
    folder(pool, "a", Some(root), "Dự án")
        .await
        .expect("another parent may reuse the name");
    exec(
        pool,
        "UPDATE folders SET state = 'TRASHED', trashed_at = now() WHERE id = $1",
        root,
    )
    .await
    .expect("trash");
    folder(pool, "a", None, "Dự án")
        .await
        .expect("a trashed folder frees its name");
}

#[tokio::test]
async fn folder_parent_must_belong_to_the_same_owner() {
    let Some(db) = TestDb::create().await else {
        return;
    };
    let pool = db.pool();
    actor(pool, "a").await;
    actor(pool, "b").await;
    let of_a = folder(pool, "a", None, "A").await.expect("folder");
    violates(folder(pool, "b", Some(of_a), "B").await, FOREIGN_KEY);
}

#[tokio::test]
async fn states_and_trash_timestamps_are_checked() {
    let Some(db) = TestDb::create().await else {
        return;
    };
    let pool = db.pool();
    actor(pool, "a").await;
    let id = folder(pool, "a", None, "A").await.expect("folder");
    let bogus = exec(pool, "UPDATE folders SET state = 'BOGUS' WHERE id = $1", id).await;
    violates(bogus, CHECK);
    let untimed = exec(
        pool,
        "UPDATE folders SET state = 'TRASHED' WHERE id = $1",
        id,
    )
    .await;
    violates(untimed, CHECK);
    let doc = draft(pool, "a").await;
    let versionless = exec(
        pool,
        "UPDATE documents SET state = 'ACTIVE' WHERE id = $1",
        doc,
    )
    .await;
    violates(versionless, CHECK);
}

#[tokio::test]
async fn versions_are_unique_per_document_and_immutable() {
    let Some(db) = TestDb::create().await else {
        return;
    };
    let pool = db.pool();
    actor(pool, "a").await;
    let doc = draft(pool, "a").await;
    let sha_a = sha('a');
    let object = object(pool, &sha_a, 10).await;
    let v = || NewVersion {
        doc,
        seq: 1,
        object,
        sha: &sha_a,
        bytes: 10,
    };
    let v1 = version(pool, v()).await.expect("v1");
    violates(version(pool, v()).await, UNIQUE);
    activate(pool, doc, v1).await.expect("activate");
    let edit = exec(
        pool,
        "UPDATE document_versions SET bytes = 1 WHERE id = $1",
        v1,
    )
    .await;
    violates(edit, RAISED);
}

#[tokio::test]
async fn current_version_must_belong_to_the_document() {
    let Some(db) = TestDb::create().await else {
        return;
    };
    let pool = db.pool();
    actor(pool, "a").await;
    let (doc, other) = (draft(pool, "a").await, draft(pool, "a").await);
    let sha_b = sha('b');
    let object = object(pool, &sha_b, 3).await;
    let foreign = NewVersion {
        doc: other,
        seq: 1,
        object,
        sha: &sha_b,
        bytes: 3,
    };
    let foreign = version(pool, foreign).await.expect("version");
    violates(activate(pool, doc, foreign).await, FOREIGN_KEY);
}

#[tokio::test]
async fn used_bytes_counts_each_content_once_and_skips_purged() {
    let Some(db) = TestDb::create().await else {
        return;
    };
    let pool = db.pool();
    actor(pool, "a").await;
    assert_eq!(quota::used_bytes(pool, "a").await.expect("quota"), 0);
    let (doc, copy, purged) = (
        draft(pool, "a").await,
        draft(pool, "a").await,
        draft(pool, "a").await,
    );
    let (c, d, e) = (sha('c'), sha('d'), sha('e'));
    let (oc, od, oe) = (
        object(pool, &c, 100).await,
        object(pool, &d, 40).await,
        object(pool, &e, 7).await,
    );
    let rows = [
        (doc, 1, oc, &c, 100),
        (doc, 2, od, &d, 40),
        (copy, 1, oc, &c, 100),
        (purged, 1, oe, &e, 7),
    ];
    for (doc, seq, object, sha, bytes) in rows {
        let row = NewVersion {
            doc,
            seq,
            object,
            sha,
            bytes,
        };
        version(pool, row).await.expect("version");
    }
    exec(
        pool,
        "UPDATE documents SET state = 'PURGED' WHERE id = $1",
        purged,
    )
    .await
    .expect("purge");
    assert_eq!(quota::used_bytes(pool, "a").await.expect("quota"), 140);
    assert_eq!(quota::used_bytes(pool, "b").await.expect("quota"), 0);
}

#[tokio::test]
async fn audit_events_are_written() {
    let Some(db) = TestDb::create().await else {
        return;
    };
    let pool = db.pool();
    let event = AuditEvent {
        actor_kind: ActorKind::User,
        actor_id: Some("a"),
        tenant_id: None,
        document_id: None,
        folder_id: None,
        action: "me.read",
        outcome: Outcome::Success,
        trace_id: "req-1",
    };
    audit::insert(pool, &event).await.expect("insert");
    let (action, result): (String, String) =
        sqlx::query_as("SELECT action, result FROM audit_events")
            .fetch_one(pool)
            .await
            .expect("row");
    assert_eq!((action.as_str(), result.as_str()), ("me.read", "success"));
}
