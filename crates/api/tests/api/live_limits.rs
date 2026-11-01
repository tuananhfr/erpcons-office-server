//! Limits of the live channel: connections per document and per address, editors per document,
//! message rate and size.
use std::time::Duration;

use office_api::LiveConfig;
use serde_json::json;
use tokio_tungstenite::tungstenite::Message;

use crate::{
    live::shared_document,
    live_support::{
        Ws, connect, connect_from, join, join_link, refused, ticket, ticket_path, wait_members,
    },
    people::{enable_link, upload},
    support::{TestApp, ok_json},
};

#[tokio::test]
async fn the_51st_connection_is_refused_and_the_11th_editor_only_views() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let (a, b, c, doc, _) = shared_document(&app).await;
    let mut open: Vec<Ws> = Vec::new();
    for n in 1..=10 {
        let (ws, hello) = join(&app, &a, &doc).await;
        assert_eq!(hello["mode"], "edit", "editor {n}");
        open.push(ws);
    }
    let (ws, hello) = join(&app, &a, &doc).await;
    assert_eq!(hello["mode"], "view");
    assert_eq!(hello["notice"], "editor_limit");
    open.push(ws);
    // One account holds at most 20 connections.
    for n in 12..=50 {
        let (ws, _) = join(&app, if n <= 30 { &b } else { &c }, &doc).await;
        open.push(ws);
    }
    wait_members(&app, &doc, 50).await;
    let t = ticket(&app, &b, &doc).await;
    let mut refused = connect(&app, &doc, &t).await;
    let ended = refused.ended().await;
    assert_eq!(ended.said.as_deref(), Some("ROOM_FULL"));
    assert_eq!(ended.code, Some(4029));
    assert_eq!(app.live().members_of(doc.parse().expect("uuid")), 50);

    // An editor leaves: the waiting one is promoted.
    let first = open.remove(0);
    first.vanish();
    let promoted = open[9].next_of("mode").await;
    assert_eq!(promoted["mode"], "edit");
    assert!(promoted["notice"].is_null());
}

#[tokio::test]
async fn one_address_has_a_connection_limit() {
    let Some(app) = TestApp::spawn_with(|s| {
        s.api.live = LiveConfig {
            max_per_address: 2,
            ..LiveConfig::default()
        };
    })
    .await
    else {
        return;
    };
    let (a, _, _, doc, _) = shared_document(&app).await;
    let (_w1, _) = join(&app, &a, &doc).await;
    let (_w2, _) = join(&app, &a, &doc).await;
    let t = ticket(&app, &a, &doc).await;
    let ended = connect(&app, &doc, &t).await.ended().await;
    assert_eq!(ended.said.as_deref(), Some("TOO_MANY_CONNECTIONS"));
    assert_eq!(ended.code, Some(4029));
}

#[tokio::test]
async fn sending_too_fast_closes_the_connection() {
    let Some(app) = TestApp::spawn_with(|s| {
        s.api.live = LiveConfig {
            message_burst: 3,
            messages_per_second: 0.01,
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
    for state in ["idle", "active", "idle", "active"] {
        wb.send_json(&json!({ "type": "presence", "v": 1, "state": state }))
            .await;
    }
    let ended = wb.ended().await;
    assert_eq!(ended.said.as_deref(), Some("rate_limited"));
    assert_eq!(ended.code, Some(4008));
    wa.presence_of(1).await;
}

#[tokio::test]
async fn an_oversized_message_closes_the_connection() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let (a, b, _, doc, _) = shared_document(&app).await;
    let (mut wa, _) = join(&app, &a, &doc).await;
    let (mut wb, hello) = join(&app, &b, &doc).await;
    let max = hello["limits"]["max_message_bytes"].as_u64().expect("max") as usize;
    assert_eq!(max, 16 * 1024);
    let padding = "x".repeat(max);
    wb.send_json(&json!({ "type": "presence", "v": 1, "state": "idle", "pad": padding }))
        .await;
    let ended = wb.ended().await;
    assert_ne!(ended.code, Some(1000));
    wa.presence_of(1).await;
    tokio::time::timeout(Duration::from_secs(5), async {
        while app.live().connections() != 1 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("left");
}

#[tokio::test]
async fn one_holder_cannot_fill_the_ticket_store() {
    let Some(app) = TestApp::spawn_with(|s| {
        s.api.live = LiveConfig {
            max_tickets: 3,
            max_tickets_per_holder: 2,
            ..LiveConfig::default()
        };
    })
    .await
    else {
        return;
    };
    let (a, b, _, doc, _) = shared_document(&app).await;
    let mut issued = Vec::new();
    for _ in 0..5 {
        issued.push(ticket(&app, &a, &doc).await);
    }
    let other = ticket(&app, &b, &doc).await;
    assert_eq!(
        refused(&app, &doc, &issued[0]).await,
        401,
        "the oldest went"
    );
    let mut newest = connect(&app, &doc, &issued[4]).await;
    newest.next_of("hello").await;
    let mut wb = connect(&app, &doc, &other).await;
    wb.next_of("hello").await;
}

#[tokio::test]
async fn one_account_has_a_connection_limit() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let (a, _, _, doc, _) = shared_document(&app).await;
    let mut open = Vec::new();
    for _ in 0..20 {
        open.push(join(&app, &a, &doc).await.0);
    }
    let t = ticket(&app, &a, &doc).await;
    let ended = connect(&app, &doc, &t).await.ended().await;
    assert_eq!(ended.said.as_deref(), Some("TOO_MANY_CONNECTIONS"));
    assert_eq!(ended.code, Some(4029));
}

#[tokio::test]
async fn link_holders_take_at_most_part_of_a_room() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let (a, _, c, doc, _) = shared_document(&app).await;
    let link = enable_link(&app, &a, &doc).await;
    let mut open = Vec::new();
    for _ in 0..30 {
        let mut ws = join_link(&app, &link, &doc).await;
        ws.next_of("hello").await;
        open.push(ws);
    }
    let ended = join_link(&app, &link, &doc).await.ended().await;
    assert_eq!(ended.said.as_deref(), Some("ROOM_FULL"));
    assert_eq!(ended.code, Some(4029));
    let (_wc, hello) = join(&app, &c, &doc).await;
    assert_eq!(hello["mode"], "view");
}

#[tokio::test]
async fn a_binary_frame_gets_an_error() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let (a, _, _, doc, _) = shared_document(&app).await;
    let (mut wa, _) = join(&app, &a, &doc).await;
    wa.send_frame(Message::Binary(vec![1, 2, 3].into())).await;
    assert_eq!(wa.next_of("error").await["code"], "INVALID_MESSAGE");
    wa.send_json(&json!({ "type": "presence", "v": 1, "state": "idle" }))
        .await;
    assert_eq!(wa.next_of("presence").await["members"][0]["state"], "idle");
}

#[tokio::test]
async fn pongs_count_towards_the_message_rate() {
    let Some(app) = TestApp::spawn_with(|s| {
        s.api.live = LiveConfig {
            message_burst: 3,
            messages_per_second: 0.01,
            ..LiveConfig::default()
        };
    })
    .await
    else {
        return;
    };
    let (a, _, _, doc, _) = shared_document(&app).await;
    let (mut wa, _) = join(&app, &a, &doc).await;
    for _ in 0..4 {
        wa.send_frame(Message::Pong(Vec::new().into())).await;
    }
    let ended = wa.ended().await;
    assert_eq!(ended.said.as_deref(), Some("rate_limited"));
    assert_eq!(ended.code, Some(4008));
}

/// A ticket for `doc` with the link's token, asked by a client the proxy saw at `from`.
async fn link_ticket_from(app: &TestApp, link: &str, doc: &str, from: &str) -> String {
    let response = app
        .http
        .post(app.url(&ticket_path(doc)))
        .header("x-link-token", link)
        .header("x-forwarded-for", from)
        .send()
        .await
        .expect("link ticket");
    let body = ok_json(response, 200).await;
    body["ticket"].as_str().expect("ticket").to_owned()
}

#[tokio::test]
async fn a_link_holder_cannot_push_out_another_ones_tickets() {
    let Some(app) = TestApp::spawn_with(|s| {
        s.api.trust_forwarded_for = true;
        s.api.live = LiveConfig {
            max_tickets_per_holder: 2,
            ..LiveConfig::default()
        };
    })
    .await
    else {
        return;
    };
    let (a, _, _, doc, _) = shared_document(&app).await;
    let link = enable_link(&app, &a, &doc).await;
    let mine = link_ticket_from(&app, &link, &doc, "203.0.113.1").await;
    for _ in 0..3 {
        link_ticket_from(&app, &link, &doc, "203.0.113.2").await;
    }
    let mut ws = connect_from(&app, &doc, &mine, "203.0.113.1").await;
    ws.next_of("hello").await;
}

/// A ticket request with `token` (a PI token, or a link's with `link`), from `from`.
async fn ticket_request_from(
    app: &TestApp,
    token: &str,
    link: bool,
    doc: &str,
    from: &str,
) -> reqwest::Response {
    let request = app.http.post(app.url(&ticket_path(doc)));
    let request = if link {
        request.header("x-link-token", token)
    } else {
        request.bearer_auth(token)
    };
    request
        .header("x-forwarded-for", from)
        .send()
        .await
        .expect("ticket")
}

async fn expect_refused_ticket(response: reqwest::Response) {
    assert_eq!(response.status(), 503);
    assert_eq!(
        response
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok()),
        Some("1")
    );
}

#[tokio::test]
async fn a_full_ipv6_56_refuses_new_tickets_without_pushing_out_others() {
    let Some(app) = TestApp::spawn_with(|s| {
        s.api.trust_forwarded_for = true;
        s.api.live = LiveConfig {
            max_tickets_per_address: 2,
            max_per_address: 2,
            ..LiveConfig::default()
        };
    })
    .await
    else {
        return;
    };
    let (a, b, _, doc, _) = shared_document(&app).await;
    let from = [
        "2001:db8:0:100::1",
        "2001:db8:0:1ff::1",
        "2001:db8:0:1a0::1",
    ];
    let response = ticket_request_from(&app, &b, false, &doc, from[0]).await;
    let b_ticket = ok_json(response, 200).await["ticket"]
        .as_str()
        .expect("ticket")
        .to_owned();
    let response = ticket_request_from(&app, &a, false, &doc, from[1]).await;
    let a_ticket = ok_json(response, 200).await["ticket"]
        .as_str()
        .expect("ticket")
        .to_owned();
    expect_refused_ticket(ticket_request_from(&app, &a, false, &doc, from[2]).await).await;
    let mut wb = connect_from(&app, &doc, &b_ticket, from[0]).await;
    wb.next_of("hello").await;
    let mut wa = connect_from(&app, &doc, &a_ticket, from[1]).await;
    wa.next_of("hello").await;

    // Connections count by the /56 too.
    let response = ticket_request_from(&app, &a, false, &doc, from[2]).await;
    let third = ok_json(response, 200).await["ticket"]
        .as_str()
        .expect("ticket")
        .to_owned();
    let ended = connect_from(&app, &doc, &third, from[2])
        .await
        .ended()
        .await;
    assert_eq!(ended.said.as_deref(), Some("TOO_MANY_CONNECTIONS"));
}

#[tokio::test]
async fn link_tickets_of_one_owner_are_capped_without_pushing_out_others() {
    let Some(app) = TestApp::spawn_with(|s| {
        s.api.trust_forwarded_for = true;
        s.api.live = LiveConfig {
            max_link_tickets_per_owner: 2,
            ..LiveConfig::default()
        };
    })
    .await
    else {
        return;
    };
    let (a, _, _, doc, _) = shared_document(&app).await;
    let (other, _) = upload(&app, &a, "second").await;
    let link = enable_link(&app, &a, &doc).await;
    let other_link = enable_link(&app, &a, &other).await;
    let mine = link_ticket_from(&app, &link, &doc, "203.0.113.1").await;
    let theirs = link_ticket_from(&app, &other_link, &other, "203.0.113.2").await;
    expect_refused_ticket(ticket_request_from(&app, &link, true, &doc, "203.0.113.1").await).await;
    expect_refused_ticket(ticket_request_from(&app, &link, true, &doc, "203.0.113.3").await).await;
    let mut ws = connect_from(&app, &other, &theirs, "203.0.113.2").await;
    ws.next_of("hello").await;
    let mut ws = connect_from(&app, &doc, &mine, "203.0.113.1").await;
    ws.next_of("hello").await;
    // Redeemed tickets make room; an account's ticket is not a link ticket.
    link_ticket_from(&app, &link, &doc, "203.0.113.3").await;
    let (_wa, hello) = join(&app, &a, &doc).await;
    assert_eq!(hello["mode"], "edit");
}
