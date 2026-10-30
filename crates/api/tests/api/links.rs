//! The public view link: opened without signing in, rate limited per address, copied again by
//! sharers, kept until turned off, replaced by turning it off and on.
use reqwest::Method;
use serde_json::{Value, json};

use crate::{
    people::{enable_link, owner, share_ok, sign_in, upload},
    support::{TestApp, docx, expect_error, ok_json},
};

async fn dialog(app: &TestApp, token: &str, doc: &str) -> Value {
    ok_json(
        app.get_as(&format!("/api/v1/documents/{doc}/permissions"), token)
            .await,
        200,
    )
    .await
}

async fn disable_link(app: &TestApp, token: &str, doc: &str) {
    let off = app
        .request(
            Method::DELETE,
            &format!("/api/v1/documents/{doc}/link"),
            token,
        )
        .send()
        .await
        .expect("disable");
    assert_eq!(off.status(), 204);
}

async fn put_link(app: &TestApp, token: &str, doc: &str, body: &Value) -> Value {
    ok_json(
        app.send_json(
            Method::PUT,
            &format!("/api/v1/documents/{doc}/link"),
            token,
            body,
        )
        .await,
        200,
    )
    .await
}

#[tokio::test]
async fn anyone_with_the_link_can_view_without_signing_in() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let a = owner(&app).await;
    let (doc, _) = upload(&app, &a, "linked").await;
    let link = enable_link(&app, &a, &doc).await;
    assert_eq!(link.len(), 43);

    let response = app.open_link(&link).await;
    assert_eq!(response.headers()["referrer-policy"], "no-referrer");
    assert_eq!(response.headers()["cache-control"], "no-store");
    let opened = ok_json(response, 200).await;
    assert_eq!(opened["id"], doc.as_str());
    assert_eq!(opened["role"], "link");
    assert_eq!(opened["folder_id"], Value::Null);
    assert_eq!(opened["shared"], true);
    let content = app.open_link_content(&link).await;
    assert_eq!(content.status(), 200);
    assert_eq!(content.headers()["x-document-version"], "1");
    assert_eq!(
        content.bytes().await.expect("bytes").to_vec(),
        docx("linked")
    );

    let dialog = ok_json(
        app.get_as(&format!("/api/v1/documents/{doc}/permissions"), &a)
            .await,
        200,
    )
    .await;
    assert_eq!(dialog["link"]["enabled"], true);
    assert_eq!(dialog["link"]["role"], "viewer");
    let views = app
        .count("SELECT count(*) FROM audit_events WHERE actor_kind = 'link' AND actor_id IS NULL")
        .await;
    assert_eq!(views, 2);
}

#[tokio::test]
async fn a_link_is_kept_until_turned_off_and_replaced_by_turning_it_on_again() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let a = owner(&app).await;
    let (doc, _) = upload(&app, &a, "rotated").await;
    let first = enable_link(&app, &a, &doc).await;
    assert_eq!(enable_link(&app, &a, &doc).await, first, "no rotation");
    assert_eq!(
        dialog(&app, &a, &doc).await["link"]["token"],
        first.as_str()
    );

    disable_link(&app, &a, &doc).await;
    expect_error(app.open_link(&first).await, 404, "NOT_FOUND", false).await;
    assert!(dialog(&app, &a, &doc).await["link"].get("token").is_none());
    let second = enable_link(&app, &a, &doc).await;
    assert_ne!(first, second);
    assert_eq!(app.open_link(&first).await.status(), 404);
    assert_eq!(app.open_link(&second).await.status(), 200);
    disable_link(&app, &a, &doc).await;
    assert_eq!(app.open_link(&second).await.status(), 404);
    // Malformed tokens get the same answer as unknown ones.
    for bad in ["short", &"A".repeat(43), &"!".repeat(43)] {
        assert_eq!(app.open_link(bad).await.status(), 404);
    }
}

#[tokio::test]
async fn a_link_can_expire() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let a = owner(&app).await;
    let (doc, _) = upload(&app, &a, "expiring").await;
    let past = app
        .send_json(
            Method::PUT,
            &format!("/api/v1/documents/{doc}/link"),
            &a,
            &json!({ "expires_at": "2020-01-01T00:00:00Z" }),
        )
        .await;
    expect_error(past, 422, "INVALID_REQUEST", false).await;
    let created = ok_json(
        app.send_json(
            Method::PUT,
            &format!("/api/v1/documents/{doc}/link"),
            &a,
            &json!({ "expires_at": "2999-01-01T00:00:00Z" }),
        )
        .await,
        200,
    )
    .await;
    assert_eq!(created["expires_at"], "2999-01-01T00:00:00Z");
    let token = created["token"].as_str().expect("token");
    assert_eq!(app.open_link(token).await.status(), 200);
    sqlx::query("UPDATE document_share_settings SET link_expires_at = now() - interval '1 second'")
        .execute(app.pool())
        .await
        .expect("expire");
    assert_eq!(app.open_link(token).await.status(), 404);
    let dialog = ok_json(
        app.get_as(&format!("/api/v1/documents/{doc}/permissions"), &a)
            .await,
        200,
    )
    .await;
    assert_eq!(
        dialog["link"],
        json!({ "enabled": false, "role": null, "expires_at": null })
    );
    // An expired link is off: turning it on makes a new token.
    let renewed = enable_link(&app, &a, &doc).await;
    assert_ne!(renewed, token);
}

#[tokio::test]
async fn turning_a_link_on_again_changes_only_its_expiry() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let a = owner(&app).await;
    let (doc, _) = upload(&app, &a, "expiry-only").await;
    let created = put_link(
        &app,
        &a,
        &doc,
        &json!({ "expires_at": "2999-01-01T00:00:00Z" }),
    )
    .await;
    let token = created["token"].as_str().expect("token").to_owned();
    let moved = put_link(
        &app,
        &a,
        &doc,
        &json!({ "expires_at": "2998-06-01T00:00:00Z" }),
    )
    .await;
    assert_eq!(moved["token"], token.as_str());
    assert_eq!(moved["expires_at"], "2998-06-01T00:00:00Z");
    let link = &dialog(&app, &a, &doc).await["link"];
    assert_eq!(link["expires_at"], "2998-06-01T00:00:00Z");
    assert_eq!(link["token"], token.as_str());
    let forever = put_link(&app, &a, &doc, &json!({})).await;
    assert_eq!(forever["token"], token.as_str());
    assert_eq!(forever["expires_at"], Value::Null);
    assert_eq!(app.open_link(&token).await.status(), 200);
}

#[tokio::test]
async fn only_people_who_may_share_see_the_link_token() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let a = owner(&app).await;
    let b = sign_in(&app, "usr_b", "Bình", "b.link@erpcons.test", true).await;
    let cm = sign_in(&app, "usr_cm", "Cúc", "cm.link@erpcons.test", true).await;
    let c = sign_in(&app, "usr_c", "Chi", "c.link@erpcons.test", true).await;
    let (doc, _) = upload(&app, &a, "who-sees").await;
    share_ok(&app, &a, &doc, &["b.link@erpcons.test"], "editor").await;
    share_ok(&app, &a, &doc, &["cm.link@erpcons.test"], "commenter").await;
    share_ok(&app, &a, &doc, &["c.link@erpcons.test"], "viewer").await;
    let token = enable_link(&app, &a, &doc).await;

    for (who, sharer) in [(&a, true), (&b, true), (&cm, false), (&c, false)] {
        let body = dialog(&app, who, &doc).await;
        assert_eq!(body["link"]["enabled"], true);
        assert_eq!(body["can_share"], sharer);
        if sharer {
            assert_eq!(body["link"]["token"], token.as_str(), "{body}");
        } else {
            assert!(body["link"].get("token").is_none(), "{body}");
        }
    }
    // An editor the owner stopped from sharing no longer sees it either.
    let settings = app
        .send_json(
            Method::PUT,
            &format!("/api/v1/documents/{doc}/share-settings"),
            &a,
            &json!({ "editors_can_share": false, "viewers_can_download": true }),
        )
        .await;
    assert_eq!(settings.status(), 200);
    let body = dialog(&app, &b, &doc).await;
    assert_eq!(body["can_share"], false);
    assert!(body["link"].get("token").is_none(), "{body}");
    // The editor's own PUT is refused, so it cannot fetch the token that way.
    let refused = app
        .request(Method::PUT, &format!("/api/v1/documents/{doc}/link"), &b)
        .send()
        .await
        .expect("put");
    assert_eq!(refused.status(), 403);
}

#[tokio::test]
async fn a_token_sealed_under_another_key_is_not_shown_and_is_replaced_on_the_next_put() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let a = owner(&app).await;
    let (doc, _) = upload(&app, &a, "rekeyed").await;
    let old = enable_link(&app, &a, &doc).await;
    // What a changed ERPCONS_LINK_KEY looks like from here: the sealed bytes no longer open.
    sqlx::query(
        "UPDATE document_share_settings
         SET link_token_sealed = set_byte(link_token_sealed, 0, get_byte(link_token_sealed, 0) # 1)",
    )
    .execute(app.pool())
    .await
    .expect("tamper");
    let body = dialog(&app, &a, &doc).await;
    assert_eq!(body["link"]["enabled"], true);
    assert!(body["link"].get("token").is_none(), "{body}");
    assert_eq!(
        app.open_link(&old).await.status(),
        200,
        "lookups use the hash, not the key"
    );
    let new = enable_link(&app, &a, &doc).await;
    assert_ne!(new, old);
    assert_eq!(dialog(&app, &a, &doc).await["link"]["token"], new.as_str());
    assert_eq!(app.open_link(&old).await.status(), 404);
}

#[tokio::test]
async fn the_token_is_never_stored_in_the_clear() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let a = owner(&app).await;
    let (doc, _) = upload(&app, &a, "at-rest").await;
    let token = enable_link(&app, &a, &doc).await;
    let leaks = app
        .count(&format!(
            "SELECT count(*) FROM document_share_settings
             WHERE position(convert_to('{token}', 'UTF8') IN link_token_sealed) > 0
                OR link_token_hash = '{token}'"
        ))
        .await;
    assert_eq!(leaks, 0);
    let audited = app
        .count(&format!(
            "SELECT count(*) FROM audit_events e WHERE e::text LIKE '%{token}%'"
        ))
        .await;
    assert_eq!(audited, 0);
}

#[tokio::test]
async fn link_views_are_limited_per_address() {
    let Some(app) = TestApp::spawn_with(|s| {
        s.api.link_rate = office_api::RateConfig {
            burst: 2,
            per_second: 0.001,
        };
    })
    .await
    else {
        return;
    };
    let a = owner(&app).await;
    let (doc, _) = upload(&app, &a, "limited").await;
    let link = enable_link(&app, &a, &doc).await;
    for _ in 0..2 {
        assert_eq!(app.open_link(&link).await.status(), 200);
    }
    let limited = app.open_link_content(&link).await;
    assert!(limited.headers().contains_key("retry-after"));
    expect_error(limited, 429, "RATE_LIMITED", true).await;
}

#[tokio::test]
async fn the_token_travels_in_a_header_never_in_the_url() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let a = owner(&app).await;
    let (doc, _) = upload(&app, &a, "header-only").await;
    let link = enable_link(&app, &a, &doc).await;
    let opened = app.open_link(&link).await;
    assert_eq!(opened.headers()["referrer-policy"], "no-referrer");
    assert_eq!(opened.status(), 200);
    // The token in a path or a query opens nothing.
    for path in [
        format!("/api/v1/links/{link}"),
        format!("/api/v1/link?token={link}"),
        "/api/v1/link".to_owned(),
        "/api/v1/link/content".to_owned(),
    ] {
        assert_eq!(app.get(&path).await.status(), 404, "{path}");
    }
}
