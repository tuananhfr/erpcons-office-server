//! Deleting old versions: the owner's alone, never the current one, and bytes freed only when no
//! other version uses them.
use office_documents::{FailPoint, jobs::PurgeConfig};
use reqwest::{Method, Response};
use serde_json::Value;

use crate::{
    people::{owner, share_ok, sign_in, upload},
    support::{TestApp, docx, etag, expect_error, new_key, ok_json, stored_objects},
    versions::{restore, used_bytes},
};

async fn delete_version(app: &TestApp, token: &str, doc: &str, seq: i64) -> Response {
    app.request(
        Method::DELETE,
        &format!("/api/v1/documents/{doc}/versions/{seq}"),
        token,
    )
    .send()
    .await
    .expect("delete version")
}

async fn history(app: &TestApp, token: &str, doc: &str) -> Vec<Value> {
    let body = ok_json(
        app.get_as(&format!("/api/v1/documents/{doc}/versions"), token)
            .await,
        200,
    )
    .await;
    body["versions"].as_array().expect("versions").clone()
}

fn seqs(history: &[Value]) -> Vec<i64> {
    history
        .iter()
        .map(|v| v["version"].as_i64().expect("seq"))
        .collect()
}

#[tokio::test]
async fn deleting_frees_only_bytes_no_other_version_uses() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let a = owner(&app).await;
    let (doc, tag) = upload(&app, &a, "first").await;
    let saved = app.put_content(&a, &doc, Some(&tag), docx("second")).await;
    assert_eq!(saved.status(), 200);
    // Version 3 is version 1 restored: both point at the same stored object.
    let restored = restore(&app, &a, &doc, "1", Some(&etag(&saved))).await;
    let current = etag(&restored);
    assert_eq!(ok_json(restored, 200).await["version"], 3);
    let objects = stored_objects(&app.store_dir());
    let used = used_bytes(&app, &a).await;
    let second_size = history(&app, &a, &doc).await[1]["size"]
        .as_u64()
        .expect("size");

    assert_eq!(delete_version(&app, &a, &doc, 2).await.status(), 204);
    assert_eq!(stored_objects(&app.store_dir()), objects - 1);
    assert_eq!(used_bytes(&app, &a).await, used - second_size);

    assert_eq!(delete_version(&app, &a, &doc, 1).await.status(), 204);
    assert_eq!(
        stored_objects(&app.store_dir()),
        objects - 1,
        "version 3 uses them"
    );
    assert_eq!(used_bytes(&app, &a).await, used - second_size);
    let bytes = app
        .get_as(&format!("/api/v1/documents/{doc}/content"), &a)
        .await
        .bytes()
        .await
        .expect("bytes");
    assert_eq!(bytes.as_ref(), docx("first").as_slice());

    expect_error(
        delete_version(&app, &a, &doc, 3).await,
        409,
        "VERSION_IS_CURRENT",
        false,
    )
    .await;
    for seq in [1, 2] {
        let content = app
            .get_as(
                &format!("/api/v1/documents/{doc}/versions/{seq}/content"),
                &a,
            )
            .await;
        expect_error(content, 404, "NOT_FOUND", false).await;
        let again = restore(&app, &a, &doc, &seq.to_string(), Some(&current)).await;
        expect_error(again, 404, "NOT_FOUND", false).await;
        expect_error(
            delete_version(&app, &a, &doc, seq).await,
            404,
            "NOT_FOUND",
            false,
        )
        .await;
    }
    assert_eq!(seqs(&history(&app, &a, &doc).await), [3]);

    // Numbers are not given again.
    let next = app
        .put_content(&a, &doc, Some(&current), docx("fourth"))
        .await;
    assert_eq!(ok_json(next, 200).await["version"], 4);
    assert_eq!(seqs(&history(&app, &a, &doc).await), [4, 3]);
    let audited = app
        .count("SELECT count(*) FROM audit_events WHERE action = 'document.version_delete' AND result = 'success'")
        .await;
    assert_eq!(audited, 2);
}

#[tokio::test]
async fn only_the_owner_deletes_and_only_while_the_document_is_active() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let a = owner(&app).await;
    let b = sign_in(&app, "usr_b", "Bình", "b@erpcons.test", true).await;
    let c = sign_in(&app, "usr_c", "Chi", "c@erpcons.test", true).await;
    let (doc, tag) = upload(&app, &a, "kept").await;
    let saved = app.put_content(&a, &doc, Some(&tag), docx("newer")).await;
    assert_eq!(saved.status(), 200);
    share_ok(&app, &a, &doc, &["b@erpcons.test"], "editor").await;

    expect_error(
        delete_version(&app, &b, &doc, 1).await,
        403,
        "FORBIDDEN",
        false,
    )
    .await;
    expect_error(
        delete_version(&app, &c, &doc, 1).await,
        404,
        "NOT_FOUND",
        false,
    )
    .await;
    expect_error(
        delete_version(&app, &a, &doc, 0).await,
        422,
        "INVALID_REQUEST",
        false,
    )
    .await;
    app.trash(&a, &format!("/api/v1/documents/{doc}")).await;
    expect_error(
        delete_version(&app, &a, &doc, 1).await,
        404,
        "NOT_FOUND",
        false,
    )
    .await;
    let kept = app.count("SELECT count(*) FROM document_versions").await;
    assert_eq!(kept, 2);
}

#[tokio::test]
async fn the_purge_job_deletes_bytes_a_deletion_left_behind() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let a = owner(&app).await;
    let (doc, tag) = upload(&app, &a, "old").await;
    let saved = app.put_content(&a, &doc, Some(&tag), docx("new")).await;
    assert_eq!(saved.status(), 200);
    let objects = stored_objects(&app.store_dir());

    app.documents()
        .fail_points()
        .arm(FailPoint::AfterVersionDelete);
    assert_eq!(delete_version(&app, &a, &doc, 1).await.status(), 204);
    assert_eq!(stored_objects(&app.store_dir()), objects);
    let pending = app
        .count("SELECT count(*) FROM storage_objects WHERE durability_state = 'DELETE_PENDING'")
        .await;
    assert_eq!(pending, 1);

    let report = app
        .documents()
        .purge(&PurgeConfig::default())
        .await
        .expect("purge");
    assert_eq!(report.objects_deleted, 1);
    assert_eq!(stored_objects(&app.store_dir()), objects - 1);

    // Purging the document afterwards still ends with nothing stored.
    app.trash(&a, &format!("/api/v1/documents/{doc}")).await;
    let path = format!("/api/v1/documents/{doc}");
    assert_eq!(app.purge(&a, &path, &new_key()).await.status(), 204);
    assert_eq!(stored_objects(&app.store_dir()), 0);
    let purged = app
        .count("SELECT count(*) FROM documents WHERE state = 'PURGED'")
        .await;
    assert_eq!(purged, 1);
}
