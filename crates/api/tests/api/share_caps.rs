//! Bounds on sharing: permissions per document, new addresses per sharer and day, and invite mail
//! per sender, per recipient and overall.
use std::time::Duration;

use office_identity::testing::DEFAULT_SUBJECT;
use reqwest::Method;
use serde_json::{Value, json};

use crate::{
    people::{OWNER_EMAIL, dialog, owner, share, share_ok, sign_in, upload},
    support::{TestApp, expect_error, ok_json},
};

/// `n` pending invitations made two days ago, outside the daily window of new recipients.
async fn old_invitations(app: &TestApp, doc: &str, prefix: &str, n: i64) {
    sqlx::query(
        "INSERT INTO document_acl (id, document_id, email_norm, role, granted_by, created_at)
         SELECT gen_random_uuid(), $1::uuid, $2 || g || '@erpcons.test', 'viewer', $3,
                now() - interval '2 days'
         FROM generate_series(1, $4) g",
    )
    .bind(doc)
    .bind(prefix)
    .bind(DEFAULT_SUBJECT)
    .bind(n)
    .execute(app.pool())
    .await
    .expect("invitations");
}

fn listed(body: &Value) -> usize {
    body["permissions"].as_array().map_or(0, Vec::len)
}

#[tokio::test]
async fn a_document_holds_at_most_500_and_the_dialog_stays_bounded() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let a = owner(&app).await;
    let c = sign_in(&app, "usr_c", "Chi", "c@erpcons.test", true).await;
    let (doc, _) = upload(&app, &a, "crowded").await;
    share_ok(&app, &a, &doc, &["c@erpcons.test"], "viewer").await;
    old_invitations(&app, &doc, "p", 498).await;

    let body = share_ok(&app, &a, &doc, &["last@erpcons.test"], "viewer").await;
    assert_eq!(body["total"], 500);
    assert_eq!(listed(&body), 500);
    let over = share(&app, &a, &doc, &["one-more@erpcons.test"], "viewer", false).await;
    expect_error(over, 422, "TOO_MANY_PERMISSIONS", false).await;
    // Changing a role of someone already there is no new permission.
    share_ok(&app, &a, &doc, &["last@erpcons.test"], "editor").await;

    // Rows written past the cap (an older release, a manual fix) still give a bounded dialog.
    old_invitations(&app, &doc, "q", 100).await;
    let body = dialog(&app, &a, &doc).await;
    assert_eq!(body["total"], 600);
    assert_eq!(listed(&body), 500);
    // A viewer's dialog is its own permission, counted alone.
    let own = dialog(&app, &c, &doc).await;
    assert_eq!(own["total"], 1);
    assert_eq!(listed(&own), 1);
}

#[tokio::test]
async fn one_sharer_adds_at_most_200_new_addresses_a_day() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let a = owner(&app).await;
    let (doc, _) = upload(&app, &a, "probing").await;
    let batch =
        |n: usize| -> Vec<String> { (0..50).map(|i| format!("r{n}-{i}@erpcons.test")).collect() };
    for n in 0..4 {
        let emails = batch(n);
        let emails: Vec<&str> = emails.iter().map(String::as_str).collect();
        share_ok(&app, &a, &doc, &emails, "viewer").await;
    }
    let over = share(&app, &a, &doc, &["r9@erpcons.test"], "viewer", false).await;
    assert!(over.headers().contains_key("retry-after"));
    expect_error(over, 429, "RATE_LIMITED", true).await;
    // Addresses already given access are not new: changing their role still works.
    let first = batch(0);
    share_ok(&app, &a, &doc, &[first[0].as_str()], "editor").await;
    // The window rolls.
    sqlx::query("UPDATE document_acl SET created_at = now() - interval '25 hours'")
        .execute(app.pool())
        .await
        .expect("age");
    share_ok(&app, &a, &doc, &["r9@erpcons.test"], "viewer").await;
}

#[tokio::test]
async fn invite_mail_needs_a_verified_sender_and_stops_at_each_cap() {
    let Some(app) = TestApp::spawn_with(|s| {
        s.api.mail = office_api::MailPolicy {
            enabled: true,
            global_daily_cap: 13,
            ..office_api::MailPolicy::default()
        };
        s.identity.positive_ttl = Duration::ZERO;
        s.identity.name_ttl = Duration::ZERO;
    })
    .await
    else {
        return;
    };
    let a = owner(&app).await;
    let outbox = || app.count("SELECT count(*) FROM mail_outbox");
    let notify = |token: &String, doc: &String, emails: Vec<&'static str>| {
        let (app, token, doc) = (&app, token.clone(), doc.clone());
        async move {
            let response = app
                .send_json(
                    Method::POST,
                    &format!("/api/v1/documents/{doc}/permissions"),
                    &token,
                    &json!({ "emails": emails, "role": "editor", "notify": true }),
                )
                .await;
            ok_json(response, 200).await["mail_skipped"].clone()
        }
    };

    // One address gets at most 10 a day, whoever sends them.
    for n in 0..11 {
        let (doc, _) = upload(&app, &a, &format!("doc-{n}")).await;
        let skipped = notify(&a, &doc, vec!["r@erpcons.test"]).await;
        assert_eq!(skipped, if n < 10 { 0 } else { 1 }, "mail {n}");
    }
    assert_eq!(outbox().await, 10);
    let payload: String = sqlx::query_scalar("SELECT payload::text FROM mail_outbox LIMIT 1")
        .fetch_one(app.pool())
        .await
        .expect("payload");
    assert!(
        payload.contains(OWNER_EMAIL),
        "the sender's verified address: {payload}"
    );

    // A sharer PI no longer vouches for sends nothing, though sharing works.
    let b = sign_in(&app, "usr_b", "Bình", "b@erpcons.test", true).await;
    let (doc, _) = upload(&app, &a, "shared").await;
    share_ok(&app, &a, &doc, &["b@erpcons.test"], "editor").await;
    app.pi.set_email("usr_b", "b@erpcons.test", false);
    assert_eq!(
        notify(&b, &doc, vec!["x1@erpcons.test", "x2@erpcons.test"]).await,
        2
    );
    assert_eq!(outbox().await, 10);

    // The overall cap (13 here) holds for everyone together.
    let skipped = notify(
        &a,
        &doc,
        vec![
            "y1@erpcons.test",
            "y2@erpcons.test",
            "y3@erpcons.test",
            "y4@erpcons.test",
        ],
    )
    .await;
    assert_eq!(skipped, 1);
    assert_eq!(outbox().await, 13);
}
