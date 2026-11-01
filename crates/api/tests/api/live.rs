//! The live room: presence, the leader and its saves, the protocol, leaving and silence.
use std::time::Duration;

use office_api::LiveConfig;
use office_identity::testing::DEFAULT_NAME;
use serde_json::{Value, json};

use crate::{
    live_support::{connect, join, link_ticket, wait_members},
    people::{enable_link, owner, share_ok, sign_in, upload},
    support::{TestApp, docx, etag, expect_error, ok_json},
};

pub const B_EMAIL: &str = "b.editor@erpcons.test";
pub const C_EMAIL: &str = "c.viewer@erpcons.test";

/// A owns a document shared with B (editor) and C (viewer); tokens of A, B, C and the doc id.
pub async fn shared_document(app: &TestApp) -> (String, String, String, String, String) {
    let a = owner(app).await;
    let b = sign_in(app, "usr_b", "Bình", B_EMAIL, true).await;
    let c = sign_in(app, "usr_c", "Chi", C_EMAIL, true).await;
    let (doc, tag) = upload(app, &a, "live").await;
    share_ok(app, &a, &doc, &[B_EMAIL], "editor").await;
    share_ok(app, &a, &doc, &[C_EMAIL], "viewer").await;
    (a, b, c, doc, tag)
}

fn names(presence: &Value) -> Vec<Option<String>> {
    presence["members"]
        .as_array()
        .expect("members")
        .iter()
        .map(|m| m["name"].as_str().map(str::to_owned))
        .collect()
}

#[tokio::test]
async fn members_join_see_each_other_and_leave() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let (a, b, c, doc, _) = shared_document(&app).await;
    let (mut wa, hello) = join(&app, &a, &doc).await;
    assert_eq!(hello["v"], 1);
    assert_eq!(hello["mode"], "edit");
    assert_eq!(hello["leader"], hello["you"]);
    assert_eq!(hello["members"][0]["kind"], "user");

    let (wb, hello_b) = join(&app, &b, &doc).await;
    assert_eq!(hello_b["mode"], "edit");
    let seen = wa.presence_of(2).await;
    assert_eq!(
        names(&seen),
        [Some(DEFAULT_NAME.into()), Some("Bình".into())]
    );
    assert_eq!(wa.next_of("leader").await["leader"], hello_b["you"]);

    let (wc, hello_c) = join(&app, &c, &doc).await;
    assert_eq!(hello_c["mode"], "view");
    assert_eq!(hello_c["leader"], hello_b["you"]);
    wa.presence_of(3).await;

    let ended = wb.bye().await;
    assert_eq!(ended.code, Some(1000));
    wa.presence_of(2).await;
    assert_eq!(wa.next_of("leader").await["leader"], hello["you"]);

    wc.vanish();
    let seen = wa.presence_of(1).await;
    assert_eq!(seen["members"][0]["id"], hello["you"]);
    wait_members(&app, &doc, 1).await;
}

#[tokio::test]
async fn a_link_holder_joins_anonymous_in_view_mode() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let (a, _, _, doc, _) = shared_document(&app).await;
    let link = enable_link(&app, &a, &doc).await;
    let (mut wa, _) = join(&app, &a, &doc).await;
    let body = ok_json(link_ticket(&app, &link, &doc).await, 200).await;
    let ticket = body["ticket"].as_str().expect("ticket");
    let mut wl = connect(&app, &doc, ticket).await;
    let hello = wl.next_of("hello").await;
    assert_eq!(hello["mode"], "view");
    assert_eq!(names(&hello), [None, None], "a link holder sees no names");
    let seen = wa.presence_of(2).await;
    assert_eq!(seen["members"][1]["kind"], "link");
    assert_eq!(names(&seen), [Some(DEFAULT_NAME.into()), None]);
    let audited = app
        .count(
            "SELECT count(*) FROM audit_events WHERE action = 'link.live' AND actor_kind = 'link'",
        )
        .await;
    assert_eq!(audited, 1);
}

#[tokio::test]
async fn while_several_edit_only_the_leader_saves_and_it_hands_over() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let (a, b, c, doc, tag) = shared_document(&app).await;

    // One editor in the room (and a viewer): saves go as without a room, even from outside it.
    let (mut wa, hello_a) = join(&app, &a, &doc).await;
    let (_wc, _) = join(&app, &c, &doc).await;
    let saved = app.put_content(&b, &doc, Some(&tag), docx("b1")).await;
    assert_eq!(saved.status(), 200);
    let tag = etag(&saved);

    let (wb, hello_b) = join(&app, &b, &doc).await;
    assert_eq!(wa.next_of("leader").await["leader"], hello_b["you"]);
    let refused = app.put_content(&a, &doc, Some(&tag), docx("a1")).await;
    let body = expect_error(refused, 409, "NOT_LEADER", false).await;
    assert!(body.get("latest_version").is_none());
    let saved = app.put_content(&b, &doc, Some(&tag), docx("b2")).await;
    assert_eq!(saved.status(), 200);
    let tag = etag(&saved);
    let versions = app
        .count(&format!(
            "SELECT count(*) FROM document_versions WHERE document_id = '{doc}'"
        ))
        .await;
    assert_eq!(versions, 3, "the refused save wrote nothing");

    // A stranger still learns nothing: 404 before the leader rule.
    let d = sign_in(&app, "usr_d", "Dũng", "d@erpcons.test", true).await;
    let stranger = app.put_content(&d, &doc, Some(&tag), docx("d")).await;
    expect_error(stranger, 404, "NOT_FOUND", false).await;

    wb.vanish();
    assert_eq!(wa.next_of("leader").await["leader"], hello_a["you"]);
    let saved = app.put_content(&a, &doc, Some(&tag), docx("a2")).await;
    assert_eq!(saved.status(), 200);
}

#[tokio::test]
async fn presence_states_relay_and_bad_messages_get_errors() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let (a, b, _, doc, _) = shared_document(&app).await;
    let (mut wa, _) = join(&app, &a, &doc).await;
    let (mut wb, _) = join(&app, &b, &doc).await;
    wa.presence_of(2).await;

    wb.send_json(&json!({ "type": "presence", "v": 1, "state": "idle" }))
        .await;
    let seen = wa.next_of("presence").await;
    assert_eq!(seen["members"][1]["state"], "idle");

    for (message, code) in [
        (
            json!({ "type": "lock.acquire", "v": 1, "region": "p1" }),
            "INVALID_MESSAGE",
        ),
        (json!({ "type": "region.changed", "v": 1 }), "UNSUPPORTED"),
        (json!({ "type": "ack", "v": 1 }), "INVALID_MESSAGE"),
        (
            json!({ "type": "presence", "v": 2, "state": "idle" }),
            "UNSUPPORTED_VERSION",
        ),
        (json!({ "type": "hello", "v": 1 }), "INVALID_MESSAGE"),
        (json!({ "v": 1 }), "INVALID_MESSAGE"),
    ] {
        wb.send_json(&message).await;
        let error = wb.next_of("error").await;
        assert_eq!(error["code"], code, "{message}");
        assert_eq!(error["v"], 1);
    }
    wb.send_text("not json".into()).await;
    assert_eq!(wb.next_of("error").await["code"], "INVALID_MESSAGE");
    // Still in the room after all that.
    wb.send_json(&json!({ "type": "presence", "v": 1, "state": "active" }))
        .await;
    assert_eq!(
        wa.next_of("presence").await["members"][1]["state"],
        "active"
    );
}

#[tokio::test]
async fn a_silent_member_is_dropped_after_the_idle_timeout() {
    let Some(app) = TestApp::spawn_with(|s| {
        s.api.live = LiveConfig {
            ping_interval: Duration::from_millis(100),
            idle_timeout: Duration::from_millis(400),
            ..LiveConfig::default()
        };
    })
    .await
    else {
        return;
    };
    let (a, b, _, doc, _) = shared_document(&app).await;
    let (mut wa, hello) = join(&app, &a, &doc).await;
    assert_eq!(hello["limits"]["ping_secs"], 0);
    // B stops reading, so it never answers a ping.
    let (mut wb, _) = join(&app, &b, &doc).await;
    wa.presence_of(2).await;
    let alone = wa.presence_of(1).await;
    assert_eq!(alone["members"][0]["id"], hello["you"]);
    let ended = wb.ended().await;
    assert_eq!(ended.said.as_deref(), Some("timeout"));
    assert_eq!(ended.code, Some(4000));
    // A, reading all along, answered the pings and is still there.
    wa.listen(Duration::from_millis(800)).await;
    assert_eq!(app.live().members_of(doc.parse().expect("uuid")), 1);
}

#[tokio::test]
async fn shutting_down_says_bye_to_everyone() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let (a, _, _, doc, _) = shared_document(&app).await;
    let (mut wa, _) = join(&app, &a, &doc).await;
    app.live().shutdown();
    let ended = wa.ended().await;
    assert_eq!(ended.said.as_deref(), Some("shutdown"));
    assert_eq!(ended.code, Some(1001));
    wait_members(&app, &doc, 0).await;
}
