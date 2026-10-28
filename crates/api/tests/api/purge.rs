//! Purge on request: an item in the trash goes for good, its quota and unshared bytes with it.
use reqwest::Method;
use serde_json::{Value, json};

use crate::support::{TestApp, docx, expect_error, new_key, ok_json, stored_objects};

fn id(v: &Value) -> &str {
    v["id"].as_str().expect("id")
}

async fn used_bytes(app: &TestApp, token: &str) -> u64 {
    let me = ok_json(app.get_as("/api/v1/me", token).await, 200).await;
    me["quota"]["used_bytes"].as_u64().expect("used_bytes")
}

async fn state_of(app: &TestApp, id: &str) -> String {
    sqlx::query_scalar("SELECT state FROM documents WHERE id = $1::uuid")
        .bind(id)
        .fetch_one(app.pool())
        .await
        .expect("state")
}

#[tokio::test]
async fn a_trashed_document_is_purged_once_and_frees_its_quota() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let token = app.token().await;
    let bytes = docx("purge me");
    let doc = ok_json(app.upload(&token, "a.docx", &bytes, &[]).await, 201).await;
    let path = format!("/api/v1/documents/{}", id(&doc));
    let active = app.purge(&token, &path, &new_key()).await;
    expect_error(active, 404, "NOT_FOUND", false).await;
    assert_eq!(used_bytes(&app, &token).await, bytes.len() as u64);

    app.trash(&token, &path).await;
    let key = new_key();
    let purged = app.purge(&token, &path, &key).await;
    assert_eq!(purged.status(), 204);
    assert!(purged.bytes().await.expect("body").is_empty());
    let replay = app.purge(&token, &path, &key).await;
    assert_eq!(replay.status(), 204, "the same key replays");
    let again = app.purge(&token, &path, &new_key()).await;
    expect_error(again, 404, "NOT_FOUND", false).await;

    expect_error(app.get_as(&path, &token).await, 404, "NOT_FOUND", false).await;
    let restore = app
        .request(Method::POST, &format!("{path}/restore"), &token)
        .send()
        .await
        .expect("restore");
    expect_error(restore, 404, "NOT_FOUND", false).await;
    let trash = ok_json(app.get_as("/api/v1/trash", &token).await, 200).await;
    assert_eq!(trash["documents"], json!([]));
    assert_eq!(state_of(&app, id(&doc)).await, "PURGED");
    assert_eq!(used_bytes(&app, &token).await, 0);
    assert_eq!(stored_objects(&app.store_dir()), 0);
    let audited = app
        .count(
            "SELECT count(*) FROM audit_events
             WHERE action = 'document.purge' AND result = 'success' AND actor_kind = 'user'",
        )
        .await;
    assert_eq!(audited, 1);
}

#[tokio::test]
async fn purging_a_document_keeps_bytes_another_one_uses() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let token = app.token().await;
    let shared = docx("shared");
    let gone = ok_json(app.upload(&token, "a.docx", &shared, &[]).await, 201).await;
    let kept = ok_json(app.upload(&token, "b.docx", &shared, &[]).await, 201).await;
    let path = format!("/api/v1/documents/{}", id(&gone));
    app.trash(&token, &path).await;
    assert_eq!(app.purge(&token, &path, &new_key()).await.status(), 204);
    assert_eq!(state_of(&app, id(&gone)).await, "PURGED");
    assert_eq!(stored_objects(&app.store_dir()), 1);
    let content = app
        .get_as(&format!("/api/v1/documents/{}/content", id(&kept)), &token)
        .await;
    assert_eq!(content.bytes().await.expect("bytes").as_ref(), shared);
    assert_eq!(used_bytes(&app, &token).await, shared.len() as u64);
}

#[tokio::test]
async fn purging_a_folder_takes_what_was_trashed_with_it() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let token = app.token().await;
    let top = app.create_folder(&token, None, "Cũ").await;
    let sub = app.create_folder(&token, Some(id(&top)), "Con").await;
    let alone = app.create_folder(&token, Some(id(&top)), "Riêng").await;
    let upload = |name: &'static str, folder: String| {
        let (app, token) = (&app, &token);
        async move {
            let r = app
                .upload(token, name, &docx(name), &[("folder_id", folder.as_str())])
                .await;
            ok_json(r, 201).await
        }
    };
    let d1 = upload("d1.docx", id(&top).to_owned()).await;
    let d2 = upload("d2.docx", id(&sub).to_owned()).await;
    let d3 = upload("d3.docx", id(&top).to_owned()).await;
    let top_path = format!("/api/v1/folders/{}", id(&top));
    expect_error(
        app.purge(&token, &top_path, &new_key()).await,
        404,
        "NOT_FOUND",
        false,
    )
    .await;
    // Trashed on their own first: they are not part of the folder's batch.
    app.trash(&token, &format!("/api/v1/documents/{}", id(&d3)))
        .await;
    app.trash(&token, &format!("/api/v1/folders/{}", id(&alone)))
        .await;
    app.trash(&token, &top_path).await;
    let sub_path = format!("/api/v1/folders/{}", id(&sub));

    assert_eq!(app.purge(&token, &top_path, &new_key()).await.status(), 204);
    for (doc, state) in [(&d1, "PURGED"), (&d2, "PURGED"), (&d3, "TRASHED")] {
        assert_eq!(state_of(&app, id(doc)).await, state);
    }
    let left: Vec<String> = sqlx::query_scalar("SELECT name FROM folders ORDER BY name")
        .fetch_all(app.pool())
        .await
        .expect("folders");
    assert_eq!(left, ["Riêng"]);
    expect_error(
        app.purge(&token, &sub_path, &new_key()).await,
        404,
        "NOT_FOUND",
        false,
    )
    .await;
    assert_eq!(stored_objects(&app.store_dir()), 1, "only d3's bytes stay");
    assert_eq!(used_bytes(&app, &token).await, docx("d3.docx").len() as u64);

    let trash = ok_json(app.get_as("/api/v1/trash", &token).await, 200).await;
    assert_eq!(trash["folders"][0]["id"], alone["id"]);
    assert_eq!(trash["folders"][0]["parent_id"], Value::Null);
    assert_eq!(trash["documents"][0]["id"], d3["id"]);
    let restored = app
        .request(
            Method::POST,
            &format!("/api/v1/documents/{}/restore", id(&d3)),
            &token,
        )
        .send()
        .await
        .expect("restore");
    let restored = ok_json(restored, 200).await;
    assert_eq!(restored["folder_id"], Value::Null, "back into the root");
    let audited: Vec<String> = sqlx::query_scalar(
        "SELECT action FROM audit_events WHERE action LIKE '%.purge' ORDER BY action",
    )
    .fetch_all(app.pool())
    .await
    .expect("audit");
    assert_eq!(
        audited,
        [
            "document.purge",
            "document.purge",
            "folder.purge",
            "folder.purge",
            "folder.purge"
        ],
        "the two refused tries are audited too"
    );
}
