use std::time::Duration;

use office_api::RateConfig;
use office_documents::FailPoint;
use reqwest::Method;
use serde_json::json;

use crate::{
    documents::doc_etag,
    rejections::chunked,
    support::{TestApp, expect_error, new_key, ok_json, pdf},
};

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

    let other = app.token_of("usr_mock_binh", "Trần Thị Bình").await;
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
async fn forwarded_for_from_a_peer_that_is_not_the_proxy_is_ignored() {
    let Some(app) = TestApp::spawn_with(|s| {
        s.api.ip_rate = RateConfig {
            burst: 1,
            per_second: 0.01,
        };
        s.api.trust_forwarded_for = true;
        // The test client connects from 127.0.0.1, which is then not the proxy.
        s.api.trusted_proxies = "10.0.0.0/8".parse().expect("proxies");
    })
    .await
    else {
        return;
    };
    assert_eq!(health_from(&app, "203.0.113.1").await, 200);
    assert_eq!(
        health_from(&app, "203.0.113.2").await,
        429,
        "a forged header does not buy a fresh bucket"
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

#[tokio::test]
async fn ipv6_clients_are_limited_by_their_64() {
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
    assert_eq!(health_from(&app, "2001:db8:1:2::1").await, 200);
    assert_eq!(
        health_from(&app, "2001:db8:1:2::ffff").await,
        429,
        "another address of the same /64 shares the bucket"
    );
    assert_eq!(health_from(&app, "2001:db8:1:3::1").await, 200);
}

/// A PDF of exactly `len` bytes.
fn pdf_of(len: usize) -> Vec<u8> {
    let mut bytes = pdf("big");
    bytes.resize(len, b' ');
    bytes
}

#[tokio::test]
async fn json_bodies_stop_at_64_kib_while_uploads_and_saves_stream_on() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let token = app.token().await;
    let mib = 1024 * 1024;
    let big_json = serde_json::to_vec(&json!({ "name": "x".repeat(mib) })).expect("json");
    // Two chunks over the limit: read and refused at 64 KiB, with no length to go by.
    let chunked_json =
        serde_json::to_vec(&json!({ "name": "x".repeat(128 * 1024) })).expect("json");
    let declared = app
        .request(Method::POST, "/api/v1/folders", &token)
        .header("idempotency-key", new_key())
        .header("content-type", "application/json")
        .body(big_json)
        .send()
        .await
        .expect("request");
    expect_error(declared, 413, "TOO_LARGE", false).await;
    let blank = app
        .request(Method::POST, "/api/v1/documents", &token)
        .header("idempotency-key", new_key())
        .header("content-type", "application/json")
        .body(chunked(chunked_json.clone()))
        .send()
        .await
        .expect("request");
    expect_error(blank, 413, "TOO_LARGE", false).await;

    let doc = ok_json(app.upload(&token, "big.pdf", &pdf_of(mib), &[]).await, 201).await;
    let id = doc["id"].as_str().expect("id");
    assert_eq!(doc["size"], mib);
    let rename = app
        .request(Method::PATCH, &format!("/api/v1/documents/{id}"), &token)
        .header("content-type", "application/json")
        .body(chunked(chunked_json))
        .send()
        .await
        .expect("request");
    expect_error(rename, 413, "TOO_LARGE", false).await;
    let tag = doc_etag(&app, &token, id).await;
    let saved = app
        .put_content(&token, id, Some(&tag), pdf_of(2 * mib))
        .await;
    assert_eq!(ok_json(saved, 200).await["size"], 2 * mib);
    assert_eq!(app.count("SELECT count(*) FROM folders").await, 0);
}

#[tokio::test]
async fn an_actor_streams_a_bounded_number_of_uploads_at_once() {
    let Some(app) = TestApp::spawn_with(|s| s.api.max_concurrent_uploads = 1).await else {
        return;
    };
    let token = app.token().await;
    let other = app.token_of("usr_mock_binh", "Trần Thị Bình").await;
    let doc = app.create_blank(&token, "pdf", None, None).await;
    let id = doc["id"].as_str().expect("id").to_owned();
    let tag = doc_etag(&app, &token, &id).await;

    let held = app.documents().fail_points().hold(FailPoint::AfterPut);
    let save = app.put_content(&token, &id, Some(&tag), pdf("v2"));
    let meanwhile = async {
        tokio::time::timeout(Duration::from_secs(10), held.reached())
            .await
            .expect("the save reaches the hold");
        let upload = app.upload(&token, "a.pdf", &pdf("a"), &[]).await;
        let retry_after = upload.headers()["retry-after"].clone();
        assert_eq!(retry_after, "1");
        expect_error(upload, 429, "RATE_LIMITED", true).await;
        let second_save = app.put_content(&token, &id, Some(&tag), pdf("v2b")).await;
        expect_error(second_save, 429, "RATE_LIMITED", true).await;
        let theirs = app.upload(&other, "b.pdf", &pdf("b"), &[]).await;
        assert_eq!(theirs.status(), 201, "another actor has its own slots");
        let blank = app
            .post_json("/api/v1/documents", &token, &json!({ "kind": "word" }))
            .await;
        assert_eq!(blank.status(), 201, "a blank document streams nothing");
        held.release();
    };
    let (saved, ()) = tokio::join!(save, meanwhile);
    assert_eq!(ok_json(saved, 200).await["version"], 2);
    let after = app.upload(&token, "a.pdf", &pdf("a"), &[]).await;
    assert_eq!(after.status(), 201, "the slot is free again");
}
