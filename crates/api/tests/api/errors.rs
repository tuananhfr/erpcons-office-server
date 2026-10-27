use crate::support::{TestApp, expect_error, request_id};

#[tokio::test]
async fn every_response_carries_a_fresh_request_id() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let first = request_id(&app.get("/api/v1/health").await);
    let second = request_id(&app.get("/api/v1/health").await);
    assert_ne!(first, second);
    assert!(uuid::Uuid::parse_str(&first).is_ok(), "{first}");
}

#[tokio::test]
async fn client_sent_request_id_is_replaced() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let response = app
        .http
        .get(app.url("/api/v1/me"))
        .header("x-request-id", "forged-id")
        .send()
        .await
        .expect("request");
    let body = expect_error(response, 401, "UNAUTHENTICATED", false).await;
    assert_ne!(body["request_id"], "forged-id");
}

#[tokio::test]
async fn unknown_paths_are_404_with_the_error_body() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    for path in ["/api/v1/nothing-here", "/elsewhere", "/api/v2/me"] {
        expect_error(app.get(path).await, 404, "NOT_FOUND", false).await;
    }
}
