use office_api::RateConfig;
use office_identity::testing::access_token_as;
use pi_mock::User;

use crate::support::{TestApp, expect_error};

const TIGHT: RateConfig = RateConfig {
    burst: 2,
    per_second: 0.01,
};

#[tokio::test]
async fn per_actor_limit_answers_429_with_retry_after() {
    let Some(app) = TestApp::spawn_with(|s| s.api.actor_rate = TIGHT).await else {
        return;
    };
    let token = app.token().await;
    for _ in 0..2 {
        assert_eq!(app.get_as("/api/v1/me", &token).await.status(), 200);
    }
    let response = app.get_as("/api/v1/me", &token).await;
    let retry_after: u64 = response.headers()["retry-after"]
        .to_str()
        .expect("ascii")
        .parse()
        .expect("whole seconds");
    assert!(retry_after >= 1);
    expect_error(response, 429, "RATE_LIMITED", true).await;

    app.pi.update_scenario(|s| {
        s.users.push(User {
            user_id: "usr_mock_binh".into(),
            display_name: "Trần Thị Bình".into(),
            email: None,
            suspended: false,
            memberships: Vec::new(),
        });
    });
    let other = access_token_as(&app.pi, "usr_mock_binh").await;
    assert_eq!(
        app.get_as("/api/v1/me", &other).await.status(),
        200,
        "another actor has its own bucket"
    );
}

#[tokio::test]
async fn per_ip_limit_applies_before_authentication() {
    let Some(app) = TestApp::spawn_with(|s| s.api.ip_rate = TIGHT).await else {
        return;
    };
    for _ in 0..2 {
        assert_eq!(app.get("/api/v1/health").await.status(), 200);
    }
    let response = app.get("/api/v1/me").await;
    assert!(response.headers().contains_key("retry-after"));
    expect_error(response, 429, "RATE_LIMITED", true).await;
}

async fn health_from(app: &TestApp, forwarded_for: &str) -> u16 {
    app.http
        .get(app.url("/api/v1/health"))
        .header("x-forwarded-for", forwarded_for)
        .send()
        .await
        .expect("request")
        .status()
        .as_u16()
}

#[tokio::test]
async fn trusted_proxy_address_keys_the_ip_limit() {
    let Some(app) = TestApp::spawn_with(|s| {
        s.api.ip_rate = RateConfig {
            burst: 1,
            per_second: 0.01,
        };
        s.api.trust_forwarded_for = true;
    })
    .await
    else {
        return;
    };
    assert_eq!(health_from(&app, "203.0.113.1").await, 200);
    assert_eq!(health_from(&app, "203.0.113.2").await, 200);
    assert_eq!(health_from(&app, "203.0.113.1").await, 429);
    assert_eq!(
        health_from(&app, "203.0.113.3, 203.0.113.1").await,
        429,
        "only the entry the proxy wrote counts"
    );
}

#[tokio::test]
async fn forwarded_for_is_ignored_unless_trusted() {
    let Some(app) = TestApp::spawn_with(|s| {
        s.api.ip_rate = RateConfig {
            burst: 1,
            per_second: 0.01,
        };
    })
    .await
    else {
        return;
    };
    assert_eq!(health_from(&app, "203.0.113.1").await, 200);
    assert_eq!(health_from(&app, "203.0.113.2").await, 429);
}

#[tokio::test]
async fn declared_oversized_body_is_413() {
    let Some(app) = TestApp::spawn_with(|s| s.api.max_body_bytes = 10).await else {
        return;
    };
    let response = app
        .http
        .post(app.url("/api/v1/me"))
        .body(vec![0u8; 11])
        .send()
        .await
        .expect("request");
    expect_error(response, 413, "TOO_LARGE", false).await;
}
