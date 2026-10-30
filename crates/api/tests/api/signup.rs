//! `/signup` and `/signup/verify`: no sign-in, generic answers, limits and the one-time link.
use office_api::{RateConfig, SignupPolicy};
use office_mail::SignupTokens;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::support::{TestApp, expect_error, expect_field_error, ok_json};

fn open(cap: u32) -> SignupPolicy {
    SignupPolicy {
        enabled: true,
        daily_cap: cap,
    }
}

fn body(name: &str, email: &str) -> Value {
    json!({ "name": name, "email": email, "password": "mật khẩu 123" })
}

async fn signup_id(app: &TestApp, email: &str) -> Uuid {
    sqlx::query_scalar("SELECT id FROM signups WHERE email = $1")
        .bind(email)
        .fetch_one(app.pool())
        .await
        .expect("sign-up")
}

#[tokio::test]
async fn closed_by_default() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let status = ok_json(app.get("/api/v1/signup").await, 200).await;
    assert_eq!(status, json!({ "open": false }));
    let response = app
        .post_public("/api/v1/signup", &body("An", "an@erpcons.test"))
        .await;
    expect_error(response, 403, "SIGNUP_CLOSED", false).await;
    assert_eq!(app.count("SELECT count(*) FROM signups").await, 0);
}

#[tokio::test]
async fn every_accepted_sign_up_gets_the_same_answer() {
    let Some(app) = TestApp::spawn_with(|s| s.api.signup = open(100)).await else {
        return;
    };
    assert_eq!(
        ok_json(app.get("/api/v1/signup").await, 200).await,
        json!({ "open": true })
    );
    let first = app
        .post_public("/api/v1/signup", &body("An", "an@erpcons.test"))
        .await;
    let first = ok_json(first, 202).await;
    // The same address again, and a bot: indistinguishable answers.
    let again = app
        .post_public("/api/v1/signup", &body("An", "AN@erpcons.test"))
        .await;
    assert_eq!(ok_json(again, 202).await, first);
    let mut bot = body("Bot", "bot@erpcons.test");
    bot["website"] = json!("http://spam.example");
    assert_eq!(
        ok_json(app.post_public("/api/v1/signup", &bot).await, 202).await,
        first
    );
    assert_eq!(first, json!({ "accepted": true }));
    assert_eq!(app.count("SELECT count(*) FROM signups").await, 1);
    assert_eq!(
        app.count("SELECT count(*) FROM mail_outbox WHERE kind = 'signup_verify'")
            .await,
        2
    );
}

#[tokio::test]
async fn invalid_fields_are_named() {
    let Some(app) = TestApp::spawn_with(|s| s.api.signup = open(100)).await else {
        return;
    };
    let cases = [
        (
            json!({ "name": " ", "email": "an@erpcons.test", "password": "12345678" }),
            "name",
        ),
        (
            json!({ "name": "An", "email": "an@", "password": "12345678" }),
            "email",
        ),
        (
            json!({ "name": "An", "email": "bính@erpcons.test", "password": "12345678" }),
            "email",
        ),
        (
            json!({ "name": "An", "email": "an@erpcons.test", "password": "1234567" }),
            "password",
        ),
    ];
    for (input, field) in cases {
        expect_field_error(app.post_public("/api/v1/signup", &input).await, field).await;
    }
    let not_json = app
        .http
        .post(app.url("/api/v1/signup"))
        .header("content-type", "application/x-www-form-urlencoded")
        .body("name=An&email=an%40erpcons.test&password=12345678")
        .send()
        .await
        .expect("request");
    expect_error(not_json, 422, "INVALID_REQUEST", false).await;
    assert_eq!(app.count("SELECT count(*) FROM signups").await, 0);
}

#[tokio::test]
async fn limits_per_address_and_per_day() {
    let Some(app) = TestApp::spawn_with(|s| {
        s.api.signup = open(2);
        s.api.signup_rate = RateConfig {
            burst: 3,
            per_second: 0.001,
        };
    })
    .await
    else {
        return;
    };
    for email in ["an@erpcons.test", "binh@erpcons.test"] {
        ok_json(
            app.post_public("/api/v1/signup", &body("A", email)).await,
            202,
        )
        .await;
    }
    let capped = app
        .post_public("/api/v1/signup", &body("Chí", "chi@erpcons.test"))
        .await;
    expect_error(capped, 429, "SIGNUP_DAILY_LIMIT", false).await;
    let limited = app
        .post_public("/api/v1/signup", &body("Dũng", "dung@erpcons.test"))
        .await;
    assert!(limited.headers().contains_key("retry-after"));
    expect_error(limited, 429, "RATE_LIMITED", true).await;
}

#[tokio::test]
async fn a_link_verifies_once() {
    let Some(app) = TestApp::spawn_with(|s| s.api.signup = open(100)).await else {
        return;
    };
    ok_json(
        app.post_public("/api/v1/signup", &body("An", "an@erpcons.test"))
            .await,
        202,
    )
    .await;
    let id = signup_id(&app, "an@erpcons.test").await;
    let token = app
        .signups()
        .issue(id)
        .await
        .expect("issue")
        .expect("pending")
        .token;
    for bad in [
        json!({}),
        json!({ "token": "x" }),
        json!({ "token": "A".repeat(43) }),
    ] {
        let response = app.post_public("/api/v1/signup/verify", &bad).await;
        expect_error(response, 400, "INVALID_TOKEN", false).await;
    }
    let ok = app
        .post_public("/api/v1/signup/verify", &json!({ "token": token }))
        .await;
    assert_eq!(ok_json(ok, 200).await, json!({ "verified": true }));
    let again = app
        .post_public("/api/v1/signup/verify", &json!({ "token": token }))
        .await;
    expect_error(again, 400, "INVALID_TOKEN", false).await;
    assert_eq!(
        app.count("SELECT count(*) FROM signups WHERE state = 'verified'")
            .await,
        1
    );
}

#[tokio::test]
async fn verification_attempts_are_limited_per_address() {
    let Some(app) = TestApp::spawn_with(|s| {
        s.api.signup_verify_rate = RateConfig {
            burst: 2,
            per_second: 0.001,
        };
    })
    .await
    else {
        return;
    };
    let guess = json!({ "token": "A".repeat(43) });
    for _ in 0..2 {
        let response = app.post_public("/api/v1/signup/verify", &guess).await;
        expect_error(response, 400, "INVALID_TOKEN", false).await;
    }
    let limited = app.post_public("/api/v1/signup/verify", &guess).await;
    expect_error(limited, 429, "RATE_LIMITED", true).await;
}
