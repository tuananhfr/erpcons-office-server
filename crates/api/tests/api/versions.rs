//! Old versions (plan v2 M1): their bytes, and restoring one as a new version.
use reqwest::{Method, Response};
use serde_json::Value;

use crate::{
    people::{owner, upload},
    support::{TestApp, docx, etag, expect_conflict, expect_error, ok_json, stored_objects},
};

pub(crate) async fn restore(
    app: &TestApp,
    token: &str,
    doc: &str,
    seq: &str,
    if_match: Option<&str>,
) -> Response {
    let mut request = app.request(
        Method::POST,
        &format!("/api/v1/documents/{doc}/versions/{seq}/restore"),
        token,
    );
    if let Some(tag) = if_match {
        request = request.header("if-match", tag);
    }
    request.send().await.expect("restore")
}

async fn bytes_of(app: &TestApp, token: &str, path: &str) -> (Vec<u8>, String) {
    let response = app.get_as(path, token).await;
    assert_eq!(response.status(), 200, "{path}");
    let version = response.headers()["x-document-version"]
        .to_str()
        .expect("ascii")
        .to_owned();
    (response.bytes().await.expect("bytes").to_vec(), version)
}

pub(crate) async fn used_bytes(app: &TestApp, token: &str) -> u64 {
    let me = ok_json(app.get_as("/api/v1/me", token).await, 200).await;
    me["quota"]["used_bytes"].as_u64().expect("used")
}

#[tokio::test]
async fn every_version_can_be_read() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let a = owner(&app).await;
    let (doc, tag) = upload(&app, &a, "v1").await;
    let saved = app.put_content(&a, &doc, Some(&tag), docx("v2")).await;
    assert_eq!(saved.status(), 200);
    for (seq, marker) in [(1, "v1"), (2, "v2")] {
        let (bytes, version) = bytes_of(
            &app,
            &a,
            &format!("/api/v1/documents/{doc}/versions/{seq}/content"),
        )
        .await;
        assert_eq!(bytes, docx(marker));
        assert_eq!(version, seq.to_string());
    }
    let missing = app
        .get_as(&format!("/api/v1/documents/{doc}/versions/3/content"), &a)
        .await;
    expect_error(missing, 404, "NOT_FOUND", false).await;
    for bad in ["0", "-1", "x"] {
        let response = app
            .get_as(
                &format!("/api/v1/documents/{doc}/versions/{bad}/content"),
                &a,
            )
            .await;
        expect_error(response, 422, "INVALID_REQUEST", false).await;
    }
}

#[tokio::test]
async fn restoring_adds_a_version_without_new_bytes() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let a = owner(&app).await;
    let (doc, tag) = upload(&app, &a, "first").await;
    let saved = app.put_content(&a, &doc, Some(&tag), docx("second")).await;
    let current = etag(&saved);
    assert_eq!(saved.status(), 200);
    let objects = stored_objects(&app.store_dir());
    let used = used_bytes(&app, &a).await;

    let response = restore(&app, &a, &doc, "1", Some(&current)).await;
    let new_tag = etag(&response);
    let restored = ok_json(response, 200).await;
    assert_eq!(restored["version"], 3);
    assert_ne!(new_tag, current);
    let (bytes, version) = bytes_of(&app, &a, &format!("/api/v1/documents/{doc}/content")).await;
    assert_eq!(bytes, docx("first"));
    assert_eq!(version, "3");
    assert_eq!(stored_objects(&app.store_dir()), objects, "no bytes copied");
    assert_eq!(used_bytes(&app, &a).await, used, "quota unchanged");
    let versions = ok_json(
        app.get_as(&format!("/api/v1/documents/{doc}/versions"), &a)
            .await,
        200,
    )
    .await;
    let list = versions["versions"].as_array().expect("versions");
    assert_eq!(list.len(), 3, "the history keeps every version");
    let sha = |v: &Value| v["sha256"].as_str().map(str::to_owned);
    assert_eq!(sha(&list[0]), sha(&list[2]));
    assert_eq!(
        list[0]["author_name"],
        office_identity::testing::DEFAULT_NAME
    );

    // A stale If-Match, as when the answer was lost and the restore is sent again: 409.
    let latest = expect_conflict(restore(&app, &a, &doc, "1", Some(&current)).await).await;
    assert_eq!(latest, 3);
    expect_error(
        restore(&app, &a, &doc, "1", None).await,
        428,
        "PRECONDITION_REQUIRED",
        false,
    )
    .await;
    expect_error(
        restore(&app, &a, &doc, "9", Some(&new_tag)).await,
        404,
        "NOT_FOUND",
        false,
    )
    .await;
    expect_error(
        restore(&app, &a, &doc, "0", Some(&new_tag)).await,
        422,
        "INVALID_REQUEST",
        false,
    )
    .await;
    // The ETag a restore answers is the one the next save names.
    let next = app
        .put_content(&a, &doc, Some(&new_tag), docx("third"))
        .await;
    assert_eq!(next.status(), 200);
    let rows = app
        .count("SELECT count(*) FROM audit_events WHERE action = 'document.version_restore' AND result = 'success'")
        .await;
    assert_eq!(rows, 1);
}

#[tokio::test]
async fn a_trashed_document_is_not_restored_to_a_version() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let a = owner(&app).await;
    let (doc, tag) = upload(&app, &a, "trashed").await;
    app.trash(&a, &format!("/api/v1/documents/{doc}")).await;
    expect_error(
        restore(&app, &a, &doc, "1", Some(&tag)).await,
        404,
        "NOT_FOUND",
        false,
    )
    .await;
}
