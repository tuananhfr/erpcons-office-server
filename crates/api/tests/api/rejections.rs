//! Requests the framework refuses before a handler runs still get the contract's `Error` body.
use futures_util::stream;
use reqwest::{Body, Method};
use serde_json::json;

use crate::support::{TestApp, expect_error, multipart, new_key};

/// A body sent in chunks with no `Content-Length`.
pub fn chunked(bytes: Vec<u8>) -> Body {
    let chunks: Vec<Result<Vec<u8>, std::io::Error>> =
        bytes.chunks(64 * 1024).map(|c| Ok(c.to_vec())).collect();
    Body::wrap_stream(stream::iter(chunks))
}

#[tokio::test]
async fn unreadable_json_path_and_query_are_invalid_requests() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let token = app.token().await;
    let folders = |body: &'static str, content_type: &'static str| {
        app.request(Method::POST, "/api/v1/folders", &token)
            .header("idempotency-key", new_key())
            .header("content-type", content_type)
            .body(body)
            .send()
    };
    for (body, content_type) in [
        ("{not json", "application/json"),
        (r#"{"name": 5}"#, "application/json"),
        (r#"{"parent_id": "nope", "name": "x"}"#, "application/json"),
        ("{}", "application/json"),
        (r#"{"name": "x"}"#, "text/plain"),
    ] {
        let response = folders(body, content_type).await.expect("request");
        expect_error(response, 422, "INVALID_REQUEST", false).await;
    }
    for path in [
        "/api/v1/documents/not-a-uuid",
        "/api/v1/documents/not-a-uuid/content",
        "/api/v1/folders/not-a-uuid/children",
        "/api/v1/folders/root/children?limit=0",
        "/api/v1/folders/root/children?limit=201",
        "/api/v1/folders/root/children?limit=abc",
        "/api/v1/folders/root/children?cursor=",
        "/api/v1/folders/root/children?cursor=%21%21garbage",
        "/api/v1/trash?cursor=c1%3Af%3Abad",
        "/api/v1/trash?limit=-1",
    ] {
        let response = app.get_as(path, &token).await;
        expect_error(response, 422, "INVALID_REQUEST", false).await;
    }
    let patch = app
        .send_json(
            Method::PATCH,
            &format!("/api/v1/documents/{}", uuid::Uuid::new_v4()),
            &token,
            &json!({ "title": ["not", "a", "string"] }),
        )
        .await;
    expect_error(patch, 422, "INVALID_REQUEST", false).await;
}

#[tokio::test]
async fn a_wrong_method_is_405_with_the_error_body() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let token = app.token().await;
    for (method, path) in [
        (Method::DELETE, "/api/v1/trash".to_owned()),
        (Method::POST, "/api/v1/health".to_owned()),
        (Method::PUT, "/api/v1/folders".to_owned()),
        (Method::GET, "/api/v1/documents".to_owned()),
        (
            Method::POST,
            format!("/api/v1/documents/{}/content", uuid::Uuid::new_v4()),
        ),
    ] {
        let response = app
            .request(method.clone(), &path, &token)
            .send()
            .await
            .expect("request");
        expect_error(response, 405, "METHOD_NOT_ALLOWED", false).await;
    }
}

#[tokio::test]
async fn chunked_bodies_over_the_limit_are_413() {
    let Some(app) = TestApp::spawn_with(|s| s.api.max_body_bytes = 4096).await else {
        return;
    };
    let token = app.token().await;
    let name = "x".repeat(8000);
    let json_body = serde_json::to_vec(&json!({ "name": name })).expect("json");
    let response = app
        .request(Method::POST, "/api/v1/folders", &token)
        .header("idempotency-key", new_key())
        .header("content-type", "application/json")
        .body(chunked(json_body))
        .send()
        .await
        .expect("request");
    expect_error(response, 413, "TOO_LARGE", false).await;

    let mut pdf = b"%PDF-1.4\n".to_vec();
    pdf.resize(20_000, b' ');
    let (content_type, form) = multipart(&[], Some(("big.pdf", &pdf)));
    let response = app
        .request(Method::POST, "/api/v1/documents", &token)
        .header("idempotency-key", new_key())
        .header("content-type", content_type)
        .body(chunked(form))
        .send()
        .await
        .expect("request");
    expect_error(response, 413, "TOO_LARGE", false).await;
    assert_eq!(app.count("SELECT count(*) FROM documents").await, 0);
}

#[tokio::test]
async fn a_chunked_save_over_the_file_limit_is_413() {
    let Some(app) = TestApp::spawn_with(|s| s.api.quota.max_file_bytes = 30_000).await else {
        return;
    };
    let token = app.token().await;
    let doc = app.create_blank(&token, "pdf", None, None).await;
    let id = doc["id"].as_str().expect("id");
    let tag = crate::documents::doc_etag(&app, &token, id).await;
    let mut big = b"%PDF-1.4\n".to_vec();
    big.resize(60_000, b' ');
    let response = app
        .request(
            Method::PUT,
            &format!("/api/v1/documents/{id}/content"),
            &token,
        )
        .header("if-match", tag)
        .body(chunked(big))
        .send()
        .await
        .expect("request");
    expect_error(response, 413, "TOO_LARGE", false).await;
    assert_eq!(app.count("SELECT count(*) FROM document_versions").await, 1);
}
