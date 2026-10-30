//! What a verified email gives: a grant only while PI confirmed it lately, the invited address in
//! the dialog (never the account's current one), and invitations that replace an expired grant.
use std::time::Duration;

use serde_json::Value;

use crate::{
    people::{dialog, grant_of, owner, share_ok, sign_in, upload},
    support::TestApp,
};

/// Every request verifies its token again, so PI's latest answer reaches the server at once.
async fn app() -> Option<TestApp> {
    TestApp::spawn_with(|s| {
        s.identity.positive_ttl = Duration::ZERO;
        s.identity.name_ttl = Duration::ZERO;
    })
    .await
}

async fn age_verification(app: &TestApp, actor: &str, days: i32) {
    sqlx::query(
        "UPDATE actor_emails SET verified_at = now() - make_interval(days => $2) WHERE actor_id = $1",
    )
    .bind(actor)
    .bind(days)
    .execute(app.pool())
    .await
    .expect("age");
}

async fn verified_long_ago(app: &TestApp, actor: &str) -> bool {
    sqlx::query_scalar(
        "SELECT verified_at < now() - interval '30 days' FROM actor_emails WHERE actor_id = $1",
    )
    .bind(actor)
    .fetch_one(app.pool())
    .await
    .expect("verified_at")
}

#[tokio::test]
async fn a_stale_verification_gets_an_invitation_not_a_grant() {
    let Some(app) = app().await else {
        return;
    };
    let a = owner(&app).await;
    let e = sign_in(&app, "usr_e", "Ế", "e@erpcons.test", true).await;
    let (doc, _) = upload(&app, &a, "stale").await;
    age_verification(&app, "usr_e", 31).await;

    let body = share_ok(&app, &a, &doc, &["e@erpcons.test"], "viewer").await;
    assert_eq!(body["permissions"][0]["pending"], true, "{body}");
    // Signing in with the address verified again takes the invitation.
    assert_eq!(app.get_as("/api/v1/me", &e).await.status(), 200);
    let path = format!("/api/v1/documents/{doc}");
    assert_eq!(app.get_as(&path, &e).await.status(), 200);
    assert_eq!(
        grant_of(&app, &a, &doc, "e@erpcons.test").await["actor_id"],
        "usr_e"
    );
}

#[tokio::test]
async fn pi_saying_nothing_about_the_email_does_not_renew_it() {
    let Some(app) = app().await else {
        return;
    };
    let e = sign_in(&app, "usr_e", "Ế", "e@erpcons.test", true).await;
    age_verification(&app, "usr_e", 31).await;
    app.pi.clear_email("usr_e");
    assert_eq!(app.get_as("/api/v1/me", &e).await.status(), 200);
    assert!(verified_long_ago(&app, "usr_e").await);
    app.pi.set_email("usr_e", "e@erpcons.test", true);
    assert_eq!(app.get_as("/api/v1/me", &e).await.status(), 200);
    assert!(!verified_long_ago(&app, "usr_e").await);
}

#[tokio::test]
async fn the_dialog_shows_the_invited_address_not_the_current_one() {
    let Some(app) = app().await else {
        return;
    };
    let a = owner(&app).await;
    let e = sign_in(&app, "usr_e", "Ế", "e.old@erpcons.test", true).await;
    let (doc, _) = upload(&app, &a, "addresses").await;
    let body = share_ok(&app, &a, &doc, &["e.old@erpcons.test"], "editor").await;
    assert_eq!(body["permissions"][0]["pending"], false);

    app.pi.set_email("usr_e", "e.new@erpcons.test", true);
    assert_eq!(app.get_as("/api/v1/me", &e).await.status(), 200);
    for viewer in [&a, &e] {
        let body = dialog(&app, viewer, &doc).await;
        assert_eq!(body["permissions"][0]["email"], "e.old@erpcons.test");
        assert_eq!(body["permissions"][0]["display_name"], "Ế");
        assert!(!body.to_string().contains("e.new@"), "{body}");
    }
}

#[tokio::test]
async fn an_invitation_replaces_an_expired_permission_of_the_same_person() {
    let Some(app) = app().await else {
        return;
    };
    let a = owner(&app).await;
    let e = sign_in(&app, "usr_e", "Ế", "e@erpcons.test", true).await;
    let (doc, _) = upload(&app, &a, "renewed").await;
    share_ok(&app, &a, &doc, &["e@erpcons.test"], "viewer").await;
    sqlx::query("UPDATE document_acl SET expires_at = now() - interval '1 second'")
        .execute(app.pool())
        .await
        .expect("expire");
    let path = format!("/api/v1/documents/{doc}");
    assert_eq!(app.get_as(&path, &e).await.status(), 404);

    // While E's verification is stale, sharing again makes an invitation beside the expired grant.
    age_verification(&app, "usr_e", 31).await;
    let body = share_ok(&app, &a, &doc, &["e@erpcons.test"], "commenter").await;
    assert_eq!(body["permissions"][0]["pending"], true, "{body}");
    // E's next sign-in takes it, instead of dropping it for the expired grant.
    assert_eq!(app.get_as("/api/v1/me", &e).await.status(), 200);
    let opened = app.get_as(&path, &e).await;
    assert_eq!(opened.status(), 200);
    let opened: Value = opened.json().await.expect("json");
    assert_eq!(opened["role"], "commenter");
    let grants = app
        .count("SELECT count(*) FROM document_acl WHERE revoked_at IS NULL AND actor_id = 'usr_e'")
        .await;
    assert_eq!(grants, 1);
}
