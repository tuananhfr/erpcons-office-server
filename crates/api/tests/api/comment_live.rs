//! `comments.changed` over a real WebSocket: to members whose role allows comments, never content.
use std::time::Duration;

use reqwest::Method;
use serde_json::{Value, json};

use crate::{
    comments::{Cast, id, reply_ok, start_ok},
    live_support::{join, join_link, wait_members},
    support::TestApp,
};

const QUIET: Duration = Duration::from_millis(400);

fn changes(messages: &[Value]) -> Vec<Value> {
    messages
        .iter()
        .filter(|m| m["type"] == "comments.changed")
        .cloned()
        .collect()
}

#[tokio::test]
async fn only_editing_and_commenting_members_hear_comment_changes() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let cast = Cast::new(&app, "live comments").await;
    let (mut wa, _) = join(&app, &cast.a, &cast.doc).await;
    let (mut wb, _) = join(&app, &cast.b, &cast.doc).await;
    let (mut wc, _) = join(&app, &cast.c, &cast.doc).await;
    let (mut wd, _) = join(&app, &cast.d, &cast.doc).await;
    let mut wl = join_link(&app, &cast.link, &cast.doc).await;
    wait_members(&app, &cast.doc, 5).await;

    let thread = start_ok(&app, &cast, &cast.c).await;
    let tid = id(&thread);
    for ws in [&mut wa, &mut wb, &mut wc] {
        let heard = ws.next_of("comments.changed").await;
        assert_eq!(
            heard,
            json!({ "type": "comments.changed", "v": 1, "thread_id": tid })
        );
    }
    reply_ok(&app, &cast, &cast.b, &tid, "Nội dung riêng").await;
    let batch = json!({ "updates": [{ "thread_id": tid, "status": "orphaned" }] });
    let anchors = app
        .send_json(Method::PATCH, &cast.path("/anchors"), &cast.c, &batch)
        .await;
    assert_eq!(anchors.status(), 200);
    let heard = changes(&wa.listen(QUIET).await);
    assert_eq!(
        heard,
        [
            json!({ "type": "comments.changed", "v": 1, "thread_id": tid }),
            json!({ "type": "comments.changed", "v": 1, "thread_id": null })
        ]
    );
    for ws in [&mut wd, &mut wl] {
        let messages = ws.listen(QUIET).await;
        assert_eq!(
            changes(&messages),
            Vec::<Value>::new(),
            "viewers hear nothing"
        );
        let text = serde_json::to_string(&messages).expect("json");
        assert!(!text.contains("Nội dung"), "{text}");
    }
    for ws in [&mut wb, &mut wc] {
        let text = serde_json::to_string(&ws.listen(QUIET).await).expect("json");
        assert!(
            !text.contains("Nội dung") && !text.contains("Xem lại"),
            "{text}"
        );
    }

    // A refused change is not announced.
    let refused = app
        .request(
            Method::POST,
            &cast.path(&format!("/{tid}/resolve")),
            &cast.c,
        )
        .send()
        .await
        .expect("resolve");
    assert_eq!(refused.status(), 403);
    assert_eq!(changes(&wa.listen(QUIET).await), Vec::<Value>::new());
}
