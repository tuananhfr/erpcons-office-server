use std::time::Duration;

use office_documents::FailPoint;
use reqwest::Method;
use serde_json::json;

use crate::support::{TestApp, docx, etag, expect_error, new_key, ok_json};

#[tokio::test]
async fn a_repeated_create_returns_the_first_response() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let token = app.token().await;
    let key = new_key();
    let body = json!({ "name": "Kế hoạch" });
    let first = ok_json(
        app.post_json_keyed("/api/v1/folders", &token, &key, &body)
            .await,
        201,
    )
    .await;
    let again = ok_json(
        app.post_json_keyed("/api/v1/folders", &token, &key, &body)
            .await,
        201,
    )
    .await;
    assert_eq!(again, first);
    assert_eq!(app.count("SELECT count(*) FROM folders").await, 1);

    let other = app
        .post_json_keyed("/api/v1/folders", &token, &key, &json!({ "name": "Khác" }))
        .await;
    expect_error(other, 422, "IDEMPOTENCY_MISMATCH", false).await;
    let blank = app
        .post_json_keyed(
            "/api/v1/documents",
            &token,
            &key,
            &json!({ "kind": "word" }),
        )
        .await;
    expect_error(blank, 422, "IDEMPOTENCY_MISMATCH", false).await;

    // Keys belong to one actor.
    let someone = app.token_of("usr_mock_binh", "Trần Thị Bình").await;
    let theirs = app
        .post_json_keyed("/api/v1/folders", &someone, &key, &body)
        .await;
    assert_ne!(ok_json(theirs, 201).await["id"], first["id"]);
}

#[tokio::test]
async fn repeated_document_creates_make_one_document() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let token = app.token().await;
    let key = new_key();
    let blank = json!({ "kind": "sheet", "title": "Bảng lương" });
    let first = app
        .post_json_keyed("/api/v1/documents", &token, &key, &blank)
        .await;
    let first_tag = etag(&first);
    let first = ok_json(first, 201).await;
    let again = app
        .post_json_keyed("/api/v1/documents", &token, &key, &blank)
        .await;
    assert_eq!(etag(&again), first_tag);
    assert_eq!(ok_json(again, 201).await, first);

    let upload_key = new_key();
    let bytes = docx("once");
    let uploaded = ok_json(
        app.upload_keyed(&token, &upload_key, "a.docx", &bytes, &[])
            .await,
        201,
    )
    .await;
    let replay = ok_json(
        app.upload_keyed(&token, &upload_key, "a.docx", &bytes, &[])
            .await,
        201,
    )
    .await;
    assert_eq!(replay, uploaded);
    let changed = app
        .upload_keyed(&token, &upload_key, "a.docx", &docx("twice"), &[])
        .await;
    expect_error(changed, 422, "IDEMPOTENCY_MISMATCH", false).await;
    assert_eq!(app.count("SELECT count(*) FROM documents").await, 2);
}

#[tokio::test]
async fn a_failed_create_frees_its_key() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let token = app.token().await;
    app.create_folder(&token, None, "Taken").await;
    let key = new_key();
    let taken = app
        .post_json_keyed("/api/v1/folders", &token, &key, &json!({ "name": "taken" }))
        .await;
    expect_error(taken, 409, "NAME_TAKEN", false).await;
    let fixed = app
        .post_json_keyed("/api/v1/folders", &token, &key, &json!({ "name": "Free" }))
        .await;
    assert_eq!(ok_json(fixed, 201).await["name"], "Free");
}

#[tokio::test]
async fn create_operations_require_one_valid_key() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let token = app.token().await;
    for path in ["/api/v1/folders", "/api/v1/documents"] {
        let body = json!({ "name": "x", "kind": "word" });
        let none = app.send_json(Method::POST, path, &token, &body).await;
        expect_error(none, 422, "INVALID_REQUEST", false).await;
        let long = "k".repeat(101);
        for key in ["", long.as_str()] {
            let response = app.post_json_keyed(path, &token, key, &body).await;
            expect_error(response, 422, "INVALID_REQUEST", false).await;
        }
        let two = app
            .request(Method::POST, path, &token)
            .header("idempotency-key", "a")
            .header("idempotency-key", "b")
            .json(&body)
            .send()
            .await
            .expect("request");
        expect_error(two, 422, "INVALID_REQUEST", false).await;
    }
    assert_eq!(app.count("SELECT count(*) FROM idempotency_keys").await, 0);
}

#[tokio::test]
async fn a_repeat_while_the_first_still_runs_is_503_then_replays() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let token = app.token().await;
    let key = new_key();
    let body = json!({ "kind": "word", "title": "Đang tạo" });
    // Paused after its bytes are stored: the key is claimed, no transaction holds a lock.
    let held = app.documents().fail_points().hold(FailPoint::AfterPut);
    let first = app.post_json_keyed("/api/v1/documents", &token, &key, &body);
    let repeat = async {
        tokio::time::timeout(Duration::from_secs(10), held.reached())
            .await
            .expect("the first request reaches the hold");
        let response = app
            .post_json_keyed("/api/v1/documents", &token, &key, &body)
            .await;
        assert_eq!(response.headers()["retry-after"], "1");
        expect_error(response, 503, "UNAVAILABLE", true).await;
        held.release();
    };
    let (first, ()) = tokio::join!(first, repeat);
    let first = ok_json(first, 201).await;
    let replay = app
        .post_json_keyed("/api/v1/documents", &token, &key, &body)
        .await;
    assert_eq!(ok_json(replay, 201).await, first);
    assert_eq!(app.count("SELECT count(*) FROM documents").await, 1);
}
