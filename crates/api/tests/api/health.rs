use serde_json::{Value, json};

use crate::support::{TestApp, expect_error};

#[tokio::test]
async fn health_is_ok_when_database_and_storage_answer() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let response = app.get("/api/v1/health").await;
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["cache-control"], "no-store");
    let body: Value = response.json().await.expect("json");
    assert_eq!(body, json!({ "status": "ok" }));
}

#[tokio::test]
async fn health_is_503_when_the_database_is_down() {
    let app = TestApp::spawn_without_database().await;
    let response = app.get("/api/v1/health").await;
    expect_error(response, 503, "UNAVAILABLE", true).await;
}

#[tokio::test]
async fn health_is_503_when_storage_is_gone() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    std::fs::remove_dir_all(app.data.path().join("store")).expect("remove store");
    let response = app.get("/api/v1/health").await;
    expect_error(response, 503, "UNAVAILABLE", true).await;
}
