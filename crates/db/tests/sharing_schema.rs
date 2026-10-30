//! Migration 0005 (sharing): its constraints, and handing invitations to the account that verifies
//! the address.
use office_db::{
    PgPool, acl, actor_emails, actors,
    audit::{self, ActorKind, AuditEvent, Outcome},
    testing::TestDb,
};
use uuid::Uuid;

const UNIQUE: &str = "23505";
const CHECK: &str = "23514";

async fn actor(pool: &PgPool, id: &str) {
    actors::upsert_seen(pool, id, "Tên").await.expect("actor");
}

async fn document(pool: &PgPool, owner: &str) -> Uuid {
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

async fn grant(
    pool: &PgPool,
    doc: Uuid,
    actor_id: Option<&str>,
    email: Option<&str>,
    role: &str,
) -> Result<Uuid, sqlx::Error> {
    let id = Uuid::now_v7();
    let mut conn = pool.acquire().await.expect("connection");
    acl::insert(
        &mut conn,
        &acl::NewPermission {
            id,
            document_id: doc,
            actor_id,
            email_norm: email,
            role,
            granted_by: "owner",
            expires_at: None,
        },
    )
    .await?;
    Ok(id)
}

fn violates<T>(result: Result<T, sqlx::Error>, sqlstate: &str) {
    match result {
        Err(sqlx::Error::Database(e)) => assert_eq!(e.code().as_deref(), Some(sqlstate), "{e}"),
        Err(e) => panic!("unexpected error: {e}"),
        Ok(_) => panic!("expected SQLSTATE {sqlstate}"),
    }
}

#[tokio::test]
async fn permissions_are_checked() {
    let Some(db) = TestDb::create().await else {
        return;
    };
    let pool = db.pool();
    for id in ["owner", "b"] {
        actor(pool, id).await;
    }
    let doc = document(pool, "owner").await;
    violates(grant(pool, doc, None, None, "viewer").await, CHECK);
    violates(grant(pool, doc, Some("b"), None, "owner").await, CHECK);
    let first = grant(pool, doc, Some("b"), None, "viewer")
        .await
        .expect("grant");
    violates(grant(pool, doc, Some("b"), None, "editor").await, UNIQUE);
    grant(pool, doc, None, Some("x@erpcons.test"), "viewer")
        .await
        .expect("invite");
    violates(
        grant(pool, doc, None, Some("x@erpcons.test"), "editor").await,
        UNIQUE,
    );
    // A revoked row frees the slot for a new one.
    let mut conn = pool.acquire().await.expect("connection");
    assert!(acl::revoke(&mut conn, doc, first).await.expect("revoke"));
    grant(pool, doc, Some("b"), None, "editor")
        .await
        .expect("granted again");
}

#[tokio::test]
async fn a_link_has_a_token_hash_and_a_sealed_token_exactly_when_it_is_on() {
    let Some(db) = TestDb::create().await else {
        return;
    };
    let pool = db.pool();
    actor(pool, "owner").await;
    let doc = document(pool, "owner").await;
    let insert = |role: Option<&'static str>,
                  hash: Option<String>,
                  nonce: Option<Vec<u8>>,
                  sealed: Option<Vec<u8>>| {
        sqlx::query(
            "INSERT INTO document_share_settings
                 (document_id, link_role, link_token_hash, link_token_nonce, link_token_sealed)
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(doc)
        .bind(role)
        .bind(hash)
        .bind(nonce)
        .bind(sealed)
        .execute(pool)
    };
    let hash = || Some("a".repeat(64));
    let nonce = || Some(vec![0u8; 12]);
    let sealed = || Some(vec![1u8; 59]);
    violates(insert(Some("viewer"), None, nonce(), sealed()).await, CHECK);
    violates(insert(None, hash(), nonce(), sealed()).await, CHECK);
    violates(insert(None, None, nonce(), None).await, CHECK);
    violates(insert(None, None, None, sealed()).await, CHECK);
    violates(insert(Some("viewer"), hash(), None, sealed()).await, CHECK);
    violates(insert(Some("viewer"), hash(), nonce(), None).await, CHECK);
    violates(
        insert(Some("viewer"), hash(), Some(vec![0u8; 11]), sealed()).await,
        CHECK,
    );
    violates(
        insert(Some("editor"), hash(), nonce(), sealed()).await,
        CHECK,
    );
    violates(
        insert(
            Some("viewer"),
            Some("not-hex".to_owned()),
            nonce(),
            sealed(),
        )
        .await,
        CHECK,
    );
    insert(Some("viewer"), hash(), nonce(), sealed())
        .await
        .expect("link on");
}

#[tokio::test]
async fn a_verified_email_takes_its_invitations() {
    let Some(db) = TestDb::create().await else {
        return;
    };
    let pool = db.pool();
    for id in ["owner", "b", "c"] {
        actor(pool, id).await;
    }
    let shared = document(pool, "owner").await;
    let own = document(pool, "b").await;
    let held = document(pool, "owner").await;
    let email = "b@erpcons.test";
    for doc in [shared, own, held] {
        grant(pool, doc, None, Some(email), "viewer")
            .await
            .expect("invite");
    }
    grant(pool, held, Some("b"), None, "editor")
        .await
        .expect("held already");
    let mut conn = pool.acquire().await.expect("connection");
    actor_emails::upsert_verified(&mut conn, "b", email)
        .await
        .expect("email");
    // Only the invitation b can take: not to its own document, not over the permission it holds.
    assert_eq!(
        acl::attach_pending(&mut conn, "b", email)
            .await
            .expect("attach"),
        1
    );
    let roles: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT document_id, role FROM document_acl
         WHERE actor_id = 'b' AND revoked_at IS NULL ORDER BY document_id",
    )
    .fetch_all(pool)
    .await
    .expect("rows");
    let mut expected = vec![(shared, "viewer".to_owned()), (held, "editor".to_owned())];
    expected.sort();
    assert_eq!(roles, expected);
    // The address moves to whoever verifies it last.
    actor_emails::upsert_verified(&mut conn, "c", email)
        .await
        .expect("moved");
    assert_eq!(
        actor_emails::actor_of(pool, email, 3600)
            .await
            .expect("lookup"),
        Some("c".to_owned())
    );
    assert_eq!(
        actor_emails::email_of(pool, "b").await.expect("lookup"),
        None
    );
}

#[tokio::test]
async fn link_views_are_audited_without_an_actor() {
    let Some(db) = TestDb::create().await else {
        return;
    };
    let event = AuditEvent {
        actor_kind: ActorKind::Link,
        actor_id: None,
        tenant_id: None,
        document_id: Some(Uuid::now_v7()),
        folder_id: None,
        action: "link.open",
        outcome: Outcome::Success,
        trace_id: "req-1",
    };
    audit::insert(db.pool(), &event).await.expect("insert");
    let kind: String = sqlx::query_scalar("SELECT actor_kind FROM audit_events")
        .fetch_one(db.pool())
        .await
        .expect("row");
    assert_eq!(kind, "link");
}

#[tokio::test]
async fn a_locked_owner_does_not_block_rows_that_name_it() {
    let Some(db) = TestDb::create().await else {
        return;
    };
    let pool = db.pool();
    actor(pool, "owner").await;
    actor(pool, "b").await;
    let doc = document(pool, "owner").await;
    // A save holds the owner's row (quota) ...
    let mut save = pool.begin().await.expect("save tx");
    assert!(actors::lock(&mut save, "owner").await.expect("lock"));
    // ... while a share by the owner writes a row whose foreign key names the owner. With `FOR
    // UPDATE` the key check would wait for the save, which in turn waits for the shared document.
    let mut share = pool.begin().await.expect("share tx");
    sqlx::query("SET LOCAL lock_timeout = '2s'")
        .execute(&mut *share)
        .await
        .expect("timeout");
    acl::insert(
        &mut share,
        &acl::NewPermission {
            id: Uuid::now_v7(),
            document_id: doc,
            actor_id: Some("b"),
            email_norm: Some("b@erpcons.test"),
            role: "viewer",
            granted_by: "owner",
            expires_at: None,
        },
    )
    .await
    .expect("the key check does not wait for the owner's lock");
    share.commit().await.expect("share");
    save.commit().await.expect("save");
}
