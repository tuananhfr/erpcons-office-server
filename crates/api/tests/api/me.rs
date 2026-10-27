use std::time::{Duration, SystemTime, UNIX_EPOCH};

use pi_mock::{DEFAULT_USER, Fault};
use reqwest::header::AUTHORIZATION;
use serde_json::{Value, json};
use time::OffsetDateTime;

use crate::support::{TestApp, expect_error, request_id};

#[tokio::test]
async fn valid_token_returns_the_actor_and_records_it() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let token = app.token().await;
    let response = app.get_as("/api/v1/me", &token).await;
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["cache-control"], "no-store");
    let trace = request_id(&response);
    let body: Value = response.json().await.expect("json");
    assert_eq!(
        body,
        json!({
            "actor_id": DEFAULT_USER,
            "display_name": "Nguyễn Văn An",
            "quota": { "used_bytes": 0, "limit_bytes": 1_073_741_824u64, "max_file_bytes": 104_857_600u64 },
        })
    );

    let (name, first_seen): (String, OffsetDateTime) =
        sqlx::query_as("SELECT display_name, last_seen_at FROM actors WHERE id = $1")
            .bind(DEFAULT_USER)
            .fetch_one(app.pool())
            .await
            .expect("actor row");
    assert_eq!(name, "Nguyễn Văn An");
    let audit: (String, String, String) =
        sqlx::query_as("SELECT actor_id, action, result FROM audit_events WHERE trace_id = $1")
            .bind(&trace)
            .fetch_one(app.pool())
            .await
            .expect("audit row");
    assert_eq!(
        audit,
        (DEFAULT_USER.into(), "me.read".into(), "success".into())
    );

    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(app.get_as("/api/v1/me", &token).await.status(), 200);
    let (count, last_seen): (i64, OffsetDateTime) =
        sqlx::query_as("SELECT count(*), max(last_seen_at) FROM actors")
            .fetch_one(app.pool())
            .await
            .expect("actors");
    assert_eq!(count, 1);
    assert!(last_seen > first_seen, "last_seen_at moves forward");
}

#[tokio::test]
async fn missing_or_malformed_authorization_is_401() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let response = app.get("/api/v1/me").await;
    assert_eq!(response.headers()["www-authenticate"], "Bearer");
    expect_error(response, 401, "UNAUTHENTICATED", false).await;
    for header in ["Basic dXNlcjpwYXNz", "Bearer", "Bearer two words"] {
        let response = app
            .http
            .get(app.url("/api/v1/me"))
            .header(AUTHORIZATION, header)
            .send()
            .await
            .expect("request");
        expect_error(response, 401, "UNAUTHENTICATED", false).await;
    }
    let calls = app
        .pi
        .requests()
        .iter()
        .filter(|r| r.path == "/v1/me")
        .count();
    assert_eq!(calls, 0, "malformed headers never reach PI");
}

#[tokio::test]
async fn token_unknown_to_pi_is_401() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let response = app.get_as("/api/v1/me", "not-issued-by-pi").await;
    expect_error(response, 401, "UNAUTHENTICATED", false).await;
    let actors: i64 = sqlx::query_scalar("SELECT count(*) FROM actors")
        .fetch_one(app.pool())
        .await
        .expect("count");
    assert_eq!(actors, 0);
}

#[tokio::test]
async fn expired_token_is_401() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let token = app.token().await;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_secs() as i64;
    app.pi.update_scenario(|s| s.now = Some(now + 2 * 3600));
    let response = app.get_as("/api/v1/me", &token).await;
    expect_error(response, 401, "UNAUTHENTICATED", false).await;
}

#[tokio::test]
async fn suspended_user_is_401() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let token = app.token().await;
    app.pi.update_scenario(|s| s.users[0].suspended = true);
    let response = app.get_as("/api/v1/me", &token).await;
    expect_error(response, 401, "UNAUTHENTICATED", false).await;
}

#[tokio::test]
async fn pi_unavailable_is_503_retryable() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let token = app.token().await;
    app.pi
        .add_fault(Fault::problem("GET", "/v1/me", 503, "unavailable"));
    let response = app.get_as("/api/v1/me", &token).await;
    expect_error(response, 503, "UNAVAILABLE", true).await;
    app.pi.clear_faults();
    assert_eq!(app.get_as("/api/v1/me", &token).await.status(), 200);
}

#[tokio::test]
async fn pi_timeout_is_503_retryable() {
    let Some(app) = TestApp::spawn_with(|s| s.identity.timeout = Duration::from_millis(300)).await
    else {
        return;
    };
    let token = app.token().await;
    app.pi
        .add_fault(Fault::delay("GET", "/v1/me", Duration::from_secs(2)));
    let response = app.get_as("/api/v1/me", &token).await;
    expect_error(response, 503, "UNAVAILABLE", true).await;
}
