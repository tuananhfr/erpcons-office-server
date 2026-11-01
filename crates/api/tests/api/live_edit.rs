//! Co-editing over a real WebSocket: region locks, updates with their seq and ack, cursors, replay,
//! saves by the leader with `X-Live-Seq`, size limits, and locks dropped when access changes.
use std::time::Duration;

use office_api::LiveConfig;
use reqwest::Method;
use serde_json::{Value, json};

use crate::{
    live::{B_EMAIL, shared_document},
    live_support::{Ws, join},
    people::{dialog, patch_permission, permission_of},
    support::{TestApp, docx, etag, expect_error},
};

fn acquire(region: &str, cid: &str) -> Value {
    json!({ "type": "lock.acquire", "v": 1, "region": region, "cid": cid })
}

fn update(region: &str, cid: &str, payload: &str) -> Value {
    json!({ "type": "region.update", "v": 1, "region": region, "cid": cid, "payload": payload })
}

fn lock_table(locks: &Value) -> Vec<String> {
    locks["locks"]
        .as_object()
        .expect("locks")
        .keys()
        .cloned()
        .collect()
}

impl Ws {
    /// Sends `message` and returns the next message of one of `kinds`.
    async fn ask(&mut self, message: &Value, kinds: &[&str]) -> Value {
        self.send_json(message).await;
        loop {
            let answer = self.next_json().await;
            if kinds.iter().any(|k| answer["type"] == *k) {
                return answer;
            }
        }
    }

    async fn lock(&mut self, region: &str, cid: &str) -> Value {
        let answer = self
            .ask(
                &acquire(region, cid),
                &["lock.granted", "lock.denied", "error"],
            )
            .await;
        assert_eq!(answer["cid"], cid, "{answer}");
        answer
    }

    async fn edit(&mut self, region: &str, cid: &str, payload: &str) -> Value {
        self.ask(&update(region, cid, payload), &["ack", "error"])
            .await
    }

    /// The next `locks` whose table is `regions`.
    async fn locks_of(&mut self, regions: &[&str]) -> Value {
        loop {
            let locks = self.next_of("locks").await;
            if lock_table(&locks) == regions {
                return locks;
            }
        }
    }

    /// The messages of `kind` heard within `period`.
    async fn heard(&mut self, kind: &str, period: Duration) -> Vec<Value> {
        self.listen(period)
            .await
            .into_iter()
            .filter(|m| m["type"] == kind)
            .collect()
    }
}

async fn save_live(
    app: &TestApp,
    token: &str,
    doc: &str,
    tag: &str,
    live_seq: &str,
    marker: &str,
) -> reqwest::Response {
    app.request(
        Method::PUT,
        &format!("/api/v1/documents/{doc}/content"),
        token,
    )
    .header("if-match", tag)
    .header("x-live-seq", live_seq)
    .body(docx(marker))
    .send()
    .await
    .expect("save")
}

#[tokio::test]
async fn locks_are_granted_denied_announced_and_dropped_on_disconnect() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let (a, b, c, doc, _) = shared_document(&app).await;
    let (mut wa, hello_a) = join(&app, &a, &doc).await;
    let (mut wb, _) = join(&app, &b, &doc).await;
    let (mut wc, _) = join(&app, &c, &doc).await;

    let granted = wa.lock("cell:s1:A1", "c1").await;
    assert_eq!(granted["type"], "lock.granted");
    assert_eq!(granted["holder"], hello_a["you"]);
    assert_eq!(granted["expires_ms"], 5000);
    let locks = wb.locks_of(&["cell:s1:A1"]).await;
    assert_eq!(locks["locks"]["cell:s1:A1"]["holder"], hello_a["you"]);
    wc.locks_of(&["cell:s1:A1"]).await;

    let denied = wb.lock("sheet:s1", "c2").await;
    assert_eq!(denied["type"], "lock.denied");
    assert_eq!(denied["holder"], hello_a["you"]);
    assert_eq!(wb.lock("*", "c3").await["type"], "lock.denied");
    assert_eq!(wb.lock("sheet:s2", "c4").await["type"], "lock.granted");

    let refused = wc.lock("cell:s1:B2", "c5").await;
    assert_eq!(refused["code"], "FORBIDDEN_MODE");

    let (mut late, _) = join(&app, &c, &doc).await;
    late.locks_of(&["cell:s1:A1", "sheet:s2"]).await;

    wa.vanish();
    wb.locks_of(&["sheet:s2"]).await;
}

#[tokio::test]
async fn an_unrenewed_lock_lapses() {
    let Some(app) = TestApp::spawn_with(|s| {
        s.api.live = LiveConfig {
            lock_ttl: Duration::from_millis(300),
            ..LiveConfig::default()
        };
    })
    .await
    else {
        return;
    };
    let (a, b, _, doc, _) = shared_document(&app).await;
    let (mut wa, _) = join(&app, &a, &doc).await;
    let (mut wb, _) = join(&app, &b, &doc).await;
    assert_eq!(wa.lock("p", "c1").await["expires_ms"], 300);
    wb.locks_of(&["p"]).await;
    wb.locks_of(&[]).await;
    assert_eq!(wb.lock("p", "c2").await["type"], "lock.granted");
}

#[tokio::test]
async fn updates_need_a_lock_get_a_seq_and_a_resent_cid_is_not_applied_twice() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let (a, b, c, doc, _) = shared_document(&app).await;
    let (mut wa, hello_a) = join(&app, &a, &doc).await;
    let (mut wb, _) = join(&app, &b, &doc).await;
    let (mut wc, _) = join(&app, &c, &doc).await;

    let refused = wa.edit("para:1", "u1", "x").await;
    assert_eq!(
        refused,
        json!({ "type": "error", "v": 1, "code": "LOCK_REQUIRED", "cid": "u1" })
    );
    wa.lock("para:1", "c1").await;
    let ack = wa.edit("para:1", "u2", "Xin chào").await;
    assert_eq!(ack["type"], "ack");
    assert_eq!(ack["cid"], "u2");
    let seq = ack["seq"].as_u64().expect("seq");
    let relayed = wb.next_of("region.update").await;
    assert_eq!(
        relayed,
        json!({ "type": "region.update", "v": 1, "seq": seq, "region": "para:1", "author": hello_a["you"], "payload": "Xin chào" })
    );
    assert_eq!(wc.next_of("region.update").await["seq"], seq);
    let next = wa.edit("para:1", "u3", "two").await["seq"]
        .as_u64()
        .expect("seq");
    assert!(next > seq);
    assert_eq!(wb.next_of("region.update").await["seq"], next);

    let refused = wc.edit("para:1", "u4", "viewer").await;
    assert_eq!(refused["code"], "FORBIDDEN_MODE");
    assert_eq!(refused["cid"], "u4");

    // A reconnect resends what it is unsure of: the same ack, nothing applied again.
    wa.vanish();
    let (mut again, _) = join(&app, &a, &doc).await;
    let ack = again.edit("para:1", "u3", "two").await;
    assert_eq!(ack["seq"], next);
    assert!(
        wb.heard("region.update", Duration::from_millis(300))
            .await
            .is_empty()
    );
}

#[tokio::test]
async fn cursors_are_relayed_but_not_from_viewers() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let (a, b, c, doc, _) = shared_document(&app).await;
    let (mut wa, _) = join(&app, &a, &doc).await;
    let (mut wb, hello_b) = join(&app, &b, &doc).await;
    let (mut wc, _) = join(&app, &c, &doc).await;
    wb.send_json(&json!({ "type": "cursor", "v": 1, "region": "para:1", "range": "2-4" }))
        .await;
    let cursor = wa.next_of("cursor").await;
    assert_eq!(cursor["author"], hello_b["you"]);
    assert_eq!(cursor["range"], "2-4");
    assert_eq!(wc.next_of("cursor").await["region"], "para:1");
    wc.send_json(&json!({ "type": "cursor", "v": 1, "region": "para:1", "range": "1" }))
        .await;
    assert_eq!(wc.next_of("error").await["code"], "FORBIDDEN_MODE");
}

#[tokio::test]
async fn a_full_log_asks_the_leader_to_save_and_its_save_trims_the_log() {
    let Some(app) = TestApp::spawn_with(|s| {
        s.api.live = LiveConfig {
            max_log_bytes: 2000,
            ..LiveConfig::default()
        };
    })
    .await
    else {
        return;
    };
    let (a, b, c, doc, tag) = shared_document(&app).await;
    let (mut wa, hello_a) = join(&app, &a, &doc).await;
    let (mut wb, _) = join(&app, &b, &doc).await;
    let saved = hello_a["saved_seq"].as_u64().expect("saved_seq");
    assert_eq!(hello_a["saved_version"], Value::Null);
    wa.lock("p", "c1").await;
    let payload = "x".repeat(900);
    let mut last = 0;
    for n in 0..3 {
        last = wa.edit("p", &format!("u{n}"), &payload).await["seq"]
            .as_u64()
            .expect("seq");
    }
    wb.next_of("save.request").await;

    let (mut wc, hello_c) = join(&app, &c, &doc).await;
    assert_eq!(hello_c["saved_seq"], saved);
    wc.send_json(&json!({ "type": "region.replay", "v": 1, "since": saved }))
        .await;
    let gone = wc.next_of("error").await;
    assert_eq!(gone["code"], "REPLAY_GONE");
    assert_eq!(gone["saved_seq"], saved);

    // Only the leader's account may save while two edit; its X-Live-Seq trims the log.
    let refused = save_live(&app, &a, &doc, &tag, &last.to_string(), "a").await;
    expect_error(refused, 409, "NOT_LEADER", false).await;
    let ok = save_live(&app, &b, &doc, &tag, &last.to_string(), "b").await;
    assert_eq!(ok.status(), 200);
    let note = wa.next_of("saved").await;
    assert_eq!(
        note,
        json!({ "type": "saved", "v": 1, "saved_seq": last, "version": 2 })
    );
    assert_eq!(wc.next_of("saved").await["saved_seq"], last);

    wc.send_json(&json!({ "type": "region.replay", "v": 1, "since": last }))
        .await;
    let done = wc.next_of("region.replayed").await;
    assert_eq!(done["since"], last);

    let after = wa.edit("p", "u9", "y").await["seq"].as_u64().expect("seq");
    wc.send_json(&json!({ "type": "region.replay", "v": 1, "since": last }))
        .await;
    let replayed = wc.next_of("region.update").await;
    assert_eq!(replayed["seq"], after);
    assert_eq!(replayed["payload"], "y");

    let (_wd, hello_d) = join(&app, &c, &doc).await;
    assert_eq!(hello_d["saved_seq"], last);
    assert_eq!(hello_d["saved_version"], 2);
}

#[tokio::test]
async fn x_live_seq_must_be_digits_and_counts_only_from_the_leader() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let (a, b, _, doc, tag) = shared_document(&app).await;
    let (mut wa, _) = join(&app, &a, &doc).await;
    wa.lock("p", "c1").await;
    let seq = wa.edit("p", "u1", "x").await["seq"].as_u64().expect("seq");

    for bad in ["", "-1", "1.5", "x", "99999999999999999999999"] {
        let response = save_live(&app, &a, &doc, &tag, bad, "bad").await;
        expect_error(response, 422, "INVALID_REQUEST", false).await;
    }
    // B edits but is not in the room: with one editor in it B may save, but not trim the log.
    let saved = save_live(&app, &b, &doc, &tag, &seq.to_string(), "b").await;
    assert_eq!(saved.status(), 200);
    let tag = etag(&saved);
    // A seq the room never issued changes nothing either.
    let saved = save_live(&app, &a, &doc, &tag, &(seq + 1_000_000).to_string(), "a").await;
    assert_eq!(saved.status(), 200);
    assert!(
        wa.heard("saved", Duration::from_millis(300))
            .await
            .is_empty()
    );
}

#[tokio::test]
async fn region_updates_may_be_large_on_edit_connections_only() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let (a, b, c, doc, _) = shared_document(&app).await;
    let (mut wa, hello_a) = join(&app, &a, &doc).await;
    let (mut wc, hello_c) = join(&app, &c, &doc).await;
    assert_eq!(hello_a["limits"]["max_message_bytes"], 16 * 1024);
    assert_eq!(hello_a["limits"]["max_region_message_bytes"], 1024 * 1024);
    assert_eq!(hello_c["limits"]["max_region_message_bytes"], 16 * 1024);

    wa.lock("p", "c1").await;
    let big = "x".repeat(200 * 1024);
    assert_eq!(wa.edit("p", "u1", &big).await["type"], "ack");
    let relayed = wc.next_of("region.update").await;
    assert_eq!(relayed["payload"].as_str().map(str::len), Some(big.len()));

    // Anything else over 16 KiB closes, even from an editor.
    let (mut wb, _) = join(&app, &b, &doc).await;
    let pad = "x".repeat(17 * 1024);
    wb.send_json(&json!({ "type": "presence", "v": 1, "state": "idle", "pad": pad }))
        .await;
    assert_eq!(wb.ended().await.code, Some(1009));

    // A viewer's connection takes no frame over 16 KiB.
    wc.send_json(&update("p", "u2", &pad)).await;
    assert_ne!(wc.ended().await.code, Some(1000));

    // Nor an editor's over 1 MiB.
    let huge = "x".repeat(1024 * 1024);
    wa.send_json(&update("p", "u3", &huge)).await;
    assert_ne!(wa.ended().await.code, Some(1000));
}

#[tokio::test]
async fn losing_edit_or_access_releases_locks() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let (a, b, _, doc, _) = shared_document(&app).await;
    let (mut wa, _) = join(&app, &a, &doc).await;
    let (mut wb, _) = join(&app, &b, &doc).await;
    wb.lock("para:1", "c1").await;
    wb.lock("para:2", "c2").await;
    wa.locks_of(&["para:1", "para:2"]).await;

    let pid_b = permission_of(&dialog(&app, &a, &doc).await, B_EMAIL);
    let body = json!({ "role": "viewer" });
    assert_eq!(patch_permission(&app, &a, &doc, &pid_b, &body).await, 200);
    assert_eq!(wb.next_of("mode").await["mode"], "view");
    wa.locks_of(&[]).await;
    assert_eq!(wb.lock("para:1", "c3").await["code"], "FORBIDDEN_MODE");

    let body = json!({ "role": "editor" });
    assert_eq!(patch_permission(&app, &a, &doc, &pid_b, &body).await, 200);
    assert_eq!(wb.next_of("mode").await["mode"], "edit");
    wb.lock("para:3", "c4").await;
    wa.locks_of(&["para:3"]).await;
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
    assert_eq!(wb.ended().await.said.as_deref(), Some("revoked"));
    wa.locks_of(&[]).await;
}
