//! Who gets into a live room and who is put out: tickets, strangers, access changes pushed at once
//! and the periodic re-check behind them.
use std::time::Duration;

use office_api::LiveConfig;
use reqwest::Method;

use crate::{
    live::{B_EMAIL, C_EMAIL, shared_document},
    live_support::{connect, join, link_ticket, refused, ticket, ticket_path, wait_members},
    people::{delete_link, dialog, enable_link, patch_permission, permission_of, sign_in},
    support::{TestApp, expect_error, ok_json},
};

#[tokio::test]
async fn strangers_get_no_ticket() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let (a, _, _, doc, _) = shared_document(&app).await;
    let d = sign_in(&app, "usr_d", "Dũng", "d@erpcons.test", true).await;
    let response = app
        .request(Method::POST, &ticket_path(&doc), &d)
        .send()
        .await
        .expect("ticket");
    expect_error(response, 404, "NOT_FOUND", false).await;
    let unknown = uuid::Uuid::new_v4().to_string();
    let response = app
        .request(Method::POST, &ticket_path(&unknown), &a)
        .send()
        .await
        .expect("ticket");
    expect_error(response, 404, "NOT_FOUND", false).await;
    let response = app
        .http
        .post(app.url(&ticket_path(&doc)))
        .send()
        .await
        .expect("ticket");
    expect_error(response, 401, "UNAUTHENTICATED", false).await;

    // A link opens its own document only, and only while it is on.
    let link = enable_link(&app, &a, &doc).await;
    let (other, _) = crate::people::upload(&app, &a, "other").await;
    expect_error(
        link_ticket(&app, &link, &other).await,
        404,
        "NOT_FOUND",
        false,
    )
    .await;
    expect_error(link_ticket(&app, "x", &doc).await, 404, "NOT_FOUND", false).await;
    assert_eq!(delete_link(&app, &a, &doc).await, 204);
    expect_error(
        link_ticket(&app, &link, &doc).await,
        404,
        "NOT_FOUND",
        false,
    )
    .await;
}

#[tokio::test]
async fn tickets_are_single_use_short_lived_and_for_one_document() {
    let Some(app) = TestApp::spawn_with(|s| {
        s.api.live = LiveConfig {
            ticket_ttl: Duration::from_millis(300),
            ..LiveConfig::default()
        };
    })
    .await
    else {
        return;
    };
    let (a, _, _, doc, _) = shared_document(&app).await;
    let (other, _) = crate::people::upload(&app, &a, "other").await;

    let response = app
        .request(Method::POST, &ticket_path(&doc), &a)
        .send()
        .await
        .expect("ticket");
    assert_eq!(response.headers()["cache-control"], "no-store");
    let body = ok_json(response, 200).await;
    let used = body["ticket"].as_str().expect("ticket").to_owned();
    assert_eq!(used.len(), 43);
    let mut ws = connect(&app, &doc, &used).await;
    ws.next_of("hello").await;
    assert_eq!(refused(&app, &doc, &used).await, 401, "reused");

    let response = app
        .request(Method::POST, &ticket_path(&doc), &a)
        .send()
        .await
        .expect("ticket");
    let expiring = ok_json(response, 200).await["ticket"]
        .as_str()
        .expect("ticket")
        .to_owned();
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(refused(&app, &doc, &expiring).await, 401, "expired");

    let response = app
        .request(Method::POST, &ticket_path(&doc), &a)
        .send()
        .await
        .expect("ticket");
    let misused = ok_json(response, 200).await["ticket"]
        .as_str()
        .expect("ticket")
        .to_owned();
    assert_eq!(refused(&app, &other, &misused).await, 401, "wrong document");
    assert_eq!(refused(&app, &doc, &misused).await, 401, "spent by the try");

    assert_eq!(refused(&app, &doc, "").await, 401);
    assert_eq!(refused(&app, &doc, &"A".repeat(43)).await, 401);
    let no_ticket = app
        .url(&format!("/api/v1/documents/{doc}/live"))
        .replacen("http://", "ws://", 1);
    match tokio_tungstenite::connect_async(no_ticket).await {
        Err(tokio_tungstenite::tungstenite::Error::Http(r)) => assert_eq!(r.status(), 401),
        other => panic!("unexpected: {:?}", other.map(|_| ())),
    }
}

#[tokio::test]
async fn a_plain_get_does_not_spend_the_ticket() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let (a, _, _, doc, _) = shared_document(&app).await;
    let t = ticket(&app, &a, &doc).await;
    let plain = app
        .get(&format!("/api/v1/documents/{doc}/live?ticket={t}"))
        .await;
    expect_error(plain, 422, "INVALID_REQUEST", false).await;
    let mut ws = connect(&app, &doc, &t).await;
    assert_eq!(ws.next_of("hello").await["mode"], "edit");
}

#[tokio::test]
async fn access_lost_meanwhile_is_checked_again_at_the_upgrade() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let (a, b, _, doc, _) = shared_document(&app).await;
    let t = ticket(&app, &b, &doc).await;
    let permissions = dialog(&app, &a, &doc).await;
    let pid = permission_of(&permissions, B_EMAIL);
    let removed = app
        .request(
            Method::DELETE,
            &format!("/api/v1/documents/{doc}/permissions/{pid}"),
            &a,
        )
        .send()
        .await
        .expect("remove");
    assert_eq!(removed.status(), 204);
    assert_eq!(refused(&app, &doc, &t).await, 404);
}

#[tokio::test]
async fn a_removed_member_is_closed_and_a_lowered_one_told_its_mode() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let (a, b, c, doc, _) = shared_document(&app).await;
    let (mut wa, _) = join(&app, &a, &doc).await;
    let (mut wb, _) = join(&app, &b, &doc).await;
    let (mut wc, hello_c) = join(&app, &c, &doc).await;
    assert_eq!(hello_c["mode"], "view");
    wa.presence_of(3).await;
    let permissions = dialog(&app, &a, &doc).await;

    let pid_b = permission_of(&permissions, B_EMAIL);
    let body = serde_json::json!({ "role": "commenter" });
    assert_eq!(patch_permission(&app, &a, &doc, &pid_b, &body).await, 200);
    let mode = wb.next_of("mode").await;
    assert_eq!(mode["mode"], "comment");
    assert!(mode["notice"].is_null());

    let body = serde_json::json!({ "role": "editor" });
    let pid_c = permission_of(&permissions, C_EMAIL);
    assert_eq!(patch_permission(&app, &a, &doc, &pid_c, &body).await, 200);
    assert_eq!(wc.next_of("mode").await["mode"], "edit");

    let removed = app
        .request(
            Method::DELETE,
            &format!("/api/v1/documents/{doc}/permissions/{pid_b}"),
            &a,
        )
        .send()
        .await
        .expect("remove");
    assert_eq!(removed.status(), 204);
    let ended = wb.ended().await;
    assert_eq!(ended.said.as_deref(), Some("revoked"));
    assert_eq!(ended.code, Some(4003));
    wa.presence_of(2).await;

    // Leaving on one's own (removing one's permission) closes one's own session too.
    let left = app
        .request(
            Method::DELETE,
            &format!("/api/v1/documents/{doc}/permissions/{pid_c}"),
            &c,
        )
        .send()
        .await
        .expect("leave");
    assert_eq!(left.status(), 204);
    assert_eq!(wc.ended().await.code, Some(4003));
    wait_members(&app, &doc, 1).await;
}

#[tokio::test]
async fn the_trash_closes_everyone_but_the_owner_who_may_only_view() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let (a, b, _, doc, _) = shared_document(&app).await;
    let link = enable_link(&app, &a, &doc).await;
    let (mut wa, _) = join(&app, &a, &doc).await;
    let (mut wb, _) = join(&app, &b, &doc).await;
    let lt = ok_json(link_ticket(&app, &link, &doc).await, 200).await;
    let mut wl = connect(&app, &doc, lt["ticket"].as_str().expect("ticket")).await;
    wl.next_of("hello").await;
    wa.presence_of(3).await;

    app.trash(&a, &format!("/api/v1/documents/{doc}")).await;
    assert_eq!(wb.ended().await.code, Some(4003));
    assert_eq!(wl.ended().await.code, Some(4003));
    assert_eq!(wa.next_of("mode").await["mode"], "view");
    wait_members(&app, &doc, 1).await;
}

#[tokio::test]
async fn turning_the_link_off_closes_link_holders_only() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let (a, b, _, doc, _) = shared_document(&app).await;
    let link = enable_link(&app, &a, &doc).await;
    let (mut wb, _) = join(&app, &b, &doc).await;
    let lt = ok_json(link_ticket(&app, &link, &doc).await, 200).await;
    let mut wl = connect(&app, &doc, lt["ticket"].as_str().expect("ticket")).await;
    wl.next_of("hello").await;
    wb.presence_of(2).await;

    assert_eq!(delete_link(&app, &a, &doc).await, 204);
    let ended = wl.ended().await;
    assert_eq!(ended.said.as_deref(), Some("revoked"));
    assert_eq!(ended.code, Some(4003));
    wb.presence_of(1).await;
    wait_members(&app, &doc, 1).await;
}

#[tokio::test]
async fn trashing_a_folder_closes_the_rooms_of_its_documents() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let (a, b, _, _, _) = shared_document(&app).await;
    let folder = app.create_folder(&a, None, "Dự án").await;
    let folder_id = folder["id"].as_str().expect("id");
    let response = app
        .upload(
            &a,
            "in-folder.docx",
            &crate::support::docx("in folder"),
            &[("folder_id", folder_id)],
        )
        .await;
    let doc = ok_json(response, 201).await["id"]
        .as_str()
        .expect("id")
        .to_owned();
    crate::people::share_ok(&app, &a, &doc, &[B_EMAIL], "editor").await;
    let (mut wb, _) = join(&app, &b, &doc).await;
    app.trash(&a, &format!("/api/v1/folders/{folder_id}")).await;
    assert_eq!(wb.ended().await.code, Some(4003));
}

#[tokio::test]
async fn the_periodic_check_catches_changes_made_elsewhere() {
    let Some(app) = TestApp::spawn_with(|s| {
        s.api.live = LiveConfig {
            recheck_interval: Duration::from_millis(200),
            ..LiveConfig::default()
        };
    })
    .await
    else {
        return;
    };
    let (a, b, c, doc, _) = shared_document(&app).await;
    let (mut wa, _) = join(&app, &a, &doc).await;
    let (mut wb, _) = join(&app, &b, &doc).await;
    let (mut wc, _) = join(&app, &c, &doc).await;
    wa.presence_of(3).await;
    // As another server process or an expiry would: no notification reaches the room.
    sqlx::query("UPDATE document_acl SET revoked_at = now() WHERE email_norm = $1")
        .bind(B_EMAIL)
        .execute(app.pool())
        .await
        .expect("revoke");
    sqlx::query("UPDATE document_acl SET role = 'commenter' WHERE email_norm = $1")
        .bind(C_EMAIL)
        .execute(app.pool())
        .await
        .expect("lower");
    assert_eq!(wb.ended().await.code, Some(4003));
    assert_eq!(wc.next_of("mode").await["mode"], "comment");
    wa.presence_of(2).await;
}

#[tokio::test]
async fn trashing_a_folder_rechecks_only_its_own_documents() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let (a, b, _, doc, _) = shared_document(&app).await;
    let (mut wb, _) = join(&app, &b, &doc).await;
    let folder = app.create_folder(&a, None, "Khác").await;
    let folder = format!("/api/v1/folders/{}", folder["id"].as_str().expect("id"));
    // Unnoticed until the periodic check: another document's change must not bring it forward.
    sqlx::query("UPDATE document_acl SET revoked_at = now() WHERE email_norm = $1")
        .bind(B_EMAIL)
        .execute(app.pool())
        .await
        .expect("revoke");
    app.trash(&a, &folder).await;
    app.trash(&a, &folder).await;
    let heard = wb.listen(Duration::from_millis(500)).await;
    assert!(heard.iter().all(|m| m["type"] != "bye"), "{heard:?}");
    wait_members(&app, &doc, 1).await;
}
