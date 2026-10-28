use std::time::{Duration, SystemTime, UNIX_EPOCH};

use office_identity::testing::{DEFAULT_NAME, DEFAULT_SUBJECT, SigningKey, segment};
use reqwest::header::AUTHORIZATION;
use serde_json::{Value, json};
use time::OffsetDateTime;

use crate::support::{TestApp, expect_error, request_id};

const JWKS: &str = "/id/jwks.json";

async fn auth_rows(app: &TestApp) -> Vec<(String, String)> {
    sqlx::query_as(
        "SELECT action, result FROM audit_events WHERE action = 'auth.verify' ORDER BY at",
    )
    .fetch_all(app.pool())
    .await
    .expect("audit rows")
}

async fn actor_count(app: &TestApp) -> i64 {
    app.count("SELECT count(*) FROM actors").await
}

/// A JWT-shaped token under PI's current kid, signed by a key PI never had.
fn forged(app: &TestApp, n: usize) -> String {
    let impostor = SigningKey::generate(&app.pi.kid());
    let claims = app.pi.claims_for(&format!("usr_forged_{n}"));
    impostor.sign(&impostor.header(), &claims)
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_secs() as i64
}

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
            "actor_id": DEFAULT_SUBJECT,
            "display_name": DEFAULT_NAME,
            "quota": { "used_bytes": 0, "limit_bytes": 1_073_741_824u64, "max_file_bytes": 104_857_600u64 },
        })
    );

    let (name, first_seen): (String, OffsetDateTime) =
        sqlx::query_as("SELECT display_name, last_seen_at FROM actors WHERE id = $1")
            .bind(DEFAULT_SUBJECT)
            .fetch_one(app.pool())
            .await
            .expect("actor row");
    assert_eq!(name, DEFAULT_NAME);
    let audit: (String, String, String) =
        sqlx::query_as("SELECT actor_id, action, result FROM audit_events WHERE trace_id = $1")
            .bind(&trace)
            .fetch_one(app.pool())
            .await
            .expect("audit row");
    assert_eq!(
        audit,
        (DEFAULT_SUBJECT.into(), "me.read".into(), "success".into())
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
    assert_eq!(
        app.pi.requests().len(),
        0,
        "malformed headers never reach PI"
    );
}

#[tokio::test]
async fn token_pi_did_not_sign_is_401() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    for token in ["not-issued-by-pi".to_owned(), forged(&app, 0)] {
        let response = app.get_as("/api/v1/me", &token).await;
        expect_error(response, 401, "UNAUTHENTICATED", false).await;
    }
    assert_eq!(actor_count(&app).await, 0);
}

#[tokio::test]
async fn unsigned_or_hmac_tokens_are_401_without_reaching_pi() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let claims = app.pi.claims_for(DEFAULT_SUBJECT);
    let kid = app.pi.kid();
    let none = format!(
        "{}.{}.",
        segment(&json!({ "alg": "none", "typ": "JWT", "kid": kid })),
        segment(&claims)
    );
    let hs256 = app.pi.sign_with_header(
        &json!({ "alg": "HS256", "typ": "JWT", "kid": kid }),
        &claims,
    );
    for token in [none, hs256] {
        let response = app.get_as("/api/v1/me", &token).await;
        expect_error(response, 401, "UNAUTHENTICATED", false).await;
    }
    assert_eq!(app.pi.requests().len(), 0);
    assert_eq!(actor_count(&app).await, 0);
}

#[tokio::test]
async fn expired_token_is_401() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let mut claims = app.pi.claims_for(DEFAULT_SUBJECT);
    claims["exp"] = json!(unix_now() - 3600);
    let response = app.get_as("/api/v1/me", &app.pi.sign(&claims)).await;
    expect_error(response, 401, "UNAUTHENTICATED", false).await;
}

#[tokio::test]
async fn tokens_of_other_clients_or_audiences_are_401() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let claims = app.pi.claims_for(DEFAULT_SUBJECT);
    let mut ubotbuild = claims.clone();
    ubotbuild["azp"] = json!("ubotbuild");
    ubotbuild["aud"] = json!(["pi-platform", "ubotbuild"]);
    let mut other_audience = claims.clone();
    other_audience["aud"] = json!(["erpcons"]);
    let mut other_issuer = claims;
    other_issuer["iss"] = json!("https://id.pi.example");
    for claims in [ubotbuild, other_audience, other_issuer] {
        let response = app.get_as("/api/v1/me", &app.pi.sign(&claims)).await;
        expect_error(response, 401, "UNAUTHENTICATED", false).await;
    }
    assert_eq!(actor_count(&app).await, 0);
}

#[tokio::test]
async fn a_locked_account_works_until_its_token_expires() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let token = app.token().await;
    app.pi.lock_user(DEFAULT_SUBJECT);
    // Accepted trade-off of local verification: PI's lock bites at the next refresh (≤ 10 min).
    assert_eq!(app.get_as("/api/v1/me", &token).await.status(), 200);
}

#[tokio::test]
async fn pi_unavailable_before_any_key_is_503_retryable() {
    let Some(app) = TestApp::spawn_with(|s| s.identity.jwks_min_refresh = Duration::ZERO).await
    else {
        return;
    };
    let token = app.token().await;
    app.pi.set_down(true);
    let response = app.get_as("/api/v1/me", &token).await;
    expect_error(response, 503, "UNAVAILABLE", true).await;
    app.pi.set_down(false);
    assert_eq!(app.get_as("/api/v1/me", &token).await.status(), 200);
}

#[tokio::test]
async fn pi_down_once_keys_are_cached_still_serves_and_keeps_names() {
    let Some(app) = TestApp::spawn_with(|s| {
        s.identity.jwks_ttl = Duration::ZERO;
        s.identity.name_ttl = Duration::ZERO;
    })
    .await
    else {
        return;
    };
    assert_eq!(
        app.get_as("/api/v1/me", &app.token().await).await.status(),
        200
    );
    app.pi.set_down(true);
    let requests = app.pi.requests().len();

    let response = app.get_as("/api/v1/me", &app.token().await).await;
    let body: Value = response.json().await.expect("json");
    assert_eq!(body["display_name"], DEFAULT_NAME, "the last known name");
    let newcomer = app.token_of("usr_mock_binh", "Trần Thị Bình").await;
    let response = app.get_as("/api/v1/me", &newcomer).await;
    let body: Value = response.json().await.expect("json");
    assert_eq!(body["display_name"], "", "PI never named this actor");
    assert!(
        app.pi.requests().len() > requests,
        "PI was tried, and was down"
    );

    let name: String = sqlx::query_scalar("SELECT display_name FROM actors WHERE id = $1")
        .bind(DEFAULT_SUBJECT)
        .fetch_one(app.pool())
        .await
        .expect("actor row");
    assert_eq!(name, DEFAULT_NAME);
}

#[tokio::test]
async fn pi_timeout_is_503_retryable() {
    let Some(app) = TestApp::spawn_with(|s| s.identity.timeout = Duration::from_millis(300)).await
    else {
        return;
    };
    let token = app.token().await;
    app.pi.set_delay(Duration::from_secs(2));
    let response = app.get_as("/api/v1/me", &token).await;
    expect_error(response, 503, "UNAVAILABLE", true).await;
}

#[tokio::test]
async fn malformed_authorization_writes_no_audit_row() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    assert_eq!(app.get("/api/v1/me").await.status(), 401);
    for header in [
        "Basic dXNlcjpwYXNz",
        "Bearer",
        "Bearer two words",
        "Bearer ===",
    ] {
        let response = app
            .http
            .get(app.url("/api/v1/me"))
            .header(AUTHORIZATION, header)
            .send()
            .await
            .expect("request");
        assert_eq!(response.status(), 401, "{header}");
    }
    let long = "a".repeat(4097);
    assert_eq!(app.get_as("/api/v1/me", &long).await.status(), 401);
    assert_eq!(app.count("SELECT count(*) FROM audit_events").await, 0);
    assert_eq!(app.pi.requests().len(), 0);
}

#[tokio::test]
async fn refused_tokens_are_audited_once_per_client_and_minute() {
    let Some(app) = TestApp::spawn_with(|s| s.identity.jwks_min_refresh = Duration::ZERO).await
    else {
        return;
    };
    for n in 0..5 {
        let response = app.get_as("/api/v1/me", &forged(&app, n)).await;
        expect_error(response, 401, "UNAUTHENTICATED", false).await;
    }
    assert_eq!(app.pi.count(JWKS), 1, "the keys were fetched once");
    assert_eq!(
        auth_rows(&app).await,
        [("auth.verify".to_owned(), "denied".to_owned())]
    );

    app.pi.rotate("2027-04", true);
    app.pi.set_down(true);
    for _ in 0..3 {
        let response = app.get_as("/api/v1/me", &app.token().await).await;
        expect_error(response, 503, "UNAVAILABLE", true).await;
    }
    assert_eq!(
        auth_rows(&app).await,
        [
            ("auth.verify".to_owned(), "denied".to_owned()),
            ("auth.verify".to_owned(), "error".to_owned())
        ],
        "an outage is sampled on its own"
    );
}

#[tokio::test]
async fn token_checks_needing_a_key_are_limited_per_client() {
    let Some(app) = TestApp::spawn_with(|s| {
        s.api.verify_rate = office_api::RateConfig {
            burst: 2,
            per_second: 0.01,
        };
    })
    .await
    else {
        return;
    };
    let token = app.token().await;
    assert_eq!(app.get_as("/api/v1/me", &token).await.status(), 200);
    let first = forged(&app, 1);
    let refused = app.get_as("/api/v1/me", &first).await;
    expect_error(refused, 401, "UNAUTHENTICATED", false).await;
    let limited = app.get_as("/api/v1/me", &forged(&app, 2)).await;
    assert!(limited.headers().contains_key("retry-after"));
    expect_error(limited, 429, "RATE_LIMITED", true).await;
    let again = app.get_as("/api/v1/me", &first).await;
    expect_error(again, 401, "UNAUTHENTICATED", false).await;
    let malformed = app.get_as("/api/v1/me", "random-token").await;
    expect_error(malformed, 401, "UNAUTHENTICATED", false).await;
    assert_eq!(
        app.get_as("/api/v1/me", &token).await.status(),
        200,
        "a cached token is not limited"
    );
    assert_eq!(app.pi.count(JWKS), 1);
}
