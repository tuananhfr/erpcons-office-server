//! Anchored comments (plan-m5): who reads and writes them, what each change does, bounds, purge.
use reqwest::{Method, Response};
use serde_json::{Value, json};

use crate::{
    people::{enable_link, owner, permission_of, share_ok, sign_in, upload},
    support::{TestApp, expect_error, new_key, ok_json},
};

pub const EDITOR_EMAIL: &str = "b.editor@erpcons.test";
pub const COMMENTER_EMAIL: &str = "c.commenter@erpcons.test";
pub const VIEWER_EMAIL: &str = "d.viewer@erpcons.test";

/// A owns `doc`; B edits, C comments, D views; E has no role; `link` is the public view link.
pub struct Cast {
    pub doc: String,
    pub a: String,
    pub b: String,
    pub c: String,
    pub d: String,
    pub e: String,
    pub link: String,
    /// The permission ids of B, C and D.
    pub grants: [String; 3],
}

impl Cast {
    pub async fn new(app: &TestApp, title: &str) -> Self {
        let a = owner(app).await;
        let (doc, _) = upload(app, &a, title).await;
        let b = sign_in(app, "usr_b", "Bình Editor", EDITOR_EMAIL, true).await;
        let c = sign_in(app, "usr_c", "Cúc Commenter", COMMENTER_EMAIL, true).await;
        let d = sign_in(app, "usr_d", "Dũng Viewer", VIEWER_EMAIL, true).await;
        let e = sign_in(app, "usr_e", "Em Stranger", "e.stranger@erpcons.test", true).await;
        share_ok(app, &a, &doc, &[EDITOR_EMAIL], "editor").await;
        share_ok(app, &a, &doc, &[COMMENTER_EMAIL], "commenter").await;
        let permissions = share_ok(app, &a, &doc, &[VIEWER_EMAIL], "viewer").await;
        let grants = [EDITOR_EMAIL, COMMENTER_EMAIL, VIEWER_EMAIL]
            .map(|email| permission_of(&permissions, email));
        let link = enable_link(app, &a, &doc).await;
        Self {
            doc,
            a,
            b,
            c,
            d,
            e,
            link,
            grants,
        }
    }

    pub fn path(&self, rest: &str) -> String {
        format!("/api/v1/documents/{}/comments{rest}", self.doc)
    }
}

pub fn anchor(region: &str) -> Value {
    json!({ "anchor": { "region": region, "offset": 3, "quote": "abc" }, "region": region, "body": "Xem lại đoạn này" })
}

pub async fn start(app: &TestApp, cast: &Cast, token: &str, body: &Value) -> Response {
    app.post_json(&cast.path(""), token, body).await
}

/// Starts a thread as `token`; returns the thread.
pub async fn start_ok(app: &TestApp, cast: &Cast, token: &str) -> Value {
    ok_json(start(app, cast, token, &anchor("p1")).await, 201).await
}

pub async fn reply_ok(app: &TestApp, cast: &Cast, token: &str, thread: &str, body: &str) -> Value {
    let response = app
        .post_json(
            &cast.path(&format!("/{thread}/replies")),
            token,
            &json!({ "body": body }),
        )
        .await;
    ok_json(response, 201).await
}

pub fn id(v: &Value) -> String {
    v["id"].as_str().expect("id").to_owned()
}

fn first_comment(thread: &Value) -> String {
    id(&thread["comments"][0])
}

async fn call(app: &TestApp, method: Method, path: &str, token: &str, body: Option<Value>) -> u16 {
    let mut request = app.request(method.clone(), path, token);
    if method == Method::POST {
        request = request.header("idempotency-key", new_key());
    }
    if let Some(body) = body {
        request = request.json(&body);
    }
    request.send().await.expect("request").status().as_u16()
}

/// Every comment route as A, B, C, D, E: the statuses each gets.
#[tokio::test]
async fn each_role_gets_what_the_matrix_says() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let cast = Cast::new(&app, "matrix").await;
    let people = [&cast.a, &cast.b, &cast.c, &cast.d, &cast.e];
    let target = start_ok(&app, &cast, &cast.a).await;
    let tid = id(&target);
    let reads = [200, 200, 200, 200, 404];
    let writes = [201, 201, 201, 403, 404];
    let changes = [200, 200, 200, 403, 404];
    for (who, (&read, (&write, &change))) in people
        .iter()
        .zip(reads.iter().zip(writes.iter().zip(&changes)))
    {
        let list = call(&app, Method::GET, &cast.path(""), who, None).await;
        assert_eq!(list, read, "list");
        let one = call(&app, Method::GET, &cast.path(&format!("/{tid}")), who, None).await;
        assert_eq!(one, read, "get");
        let started = call(&app, Method::POST, &cast.path(""), who, Some(anchor("p2"))).await;
        assert_eq!(started, write, "create");
        let reply = json!({ "body": "Đồng ý" });
        let replied = call(
            &app,
            Method::POST,
            &cast.path(&format!("/{tid}/replies")),
            who,
            Some(reply),
        )
        .await;
        assert_eq!(replied, write, "reply");
        let batch = json!({ "updates": [{ "thread_id": tid, "status": "open" }] });
        let anchors = call(
            &app,
            Method::PATCH,
            &cast.path("/anchors"),
            who,
            Some(batch),
        )
        .await;
        assert_eq!(anchors, change, "anchors");
        // D and E try A's first comment; the others their own reply.
        let own = if change == 200 {
            let thread = ok_json(app.get_as(&cast.path(&format!("/{tid}")), who).await, 200).await;
            thread["comments"]
                .as_array()
                .expect("comments")
                .iter()
                .rev()
                .find(|c| c["mine"] == true)
                .map(id)
                .expect("own reply")
        } else {
            first_comment(&target)
        };
        let comment = cast.path(&format!("/{tid}/comments/{own}"));
        let edit = json!({ "body": "Sửa lại" });
        let edited = call(&app, Method::PATCH, &comment, who, Some(edit)).await;
        assert_eq!(edited, change, "edit");
        let deleted = call(&app, Method::DELETE, &comment, who, None).await;
        assert_eq!(deleted, if change == 200 { 204 } else { change }, "delete");
    }
    let moderate = [200, 200, 403, 403, 404];
    let delete_thread = [204, 204, 403, 403, 404];
    for (who, (&expected, &delete)) in people.iter().zip(moderate.iter().zip(&delete_thread)) {
        let thread = start_ok(&app, &cast, &cast.c).await;
        let t = id(&thread);
        let resolve = call(
            &app,
            Method::POST,
            &cast.path(&format!("/{t}/resolve")),
            who,
            None,
        )
        .await;
        assert_eq!(resolve, expected, "resolve");
        if expected == 200 {
            assert_eq!(
                call(
                    &app,
                    Method::POST,
                    &cast.path(&format!("/{t}/reopen")),
                    who,
                    None
                )
                .await,
                200
            );
        } else {
            let a = &cast.a;
            assert_eq!(
                call(
                    &app,
                    Method::POST,
                    &cast.path(&format!("/{t}/resolve")),
                    a,
                    None
                )
                .await,
                200
            );
            let reopen = call(
                &app,
                Method::POST,
                &cast.path(&format!("/{t}/reopen")),
                who,
                None,
            )
            .await;
            assert_eq!(reopen, expected, "reopen");
        }
        let gone = call(
            &app,
            Method::DELETE,
            &cast.path(&format!("/{t}")),
            who,
            None,
        )
        .await;
        assert_eq!(gone, delete, "delete thread");
    }
}

/// A GET of `path` with the link token and no PI token.
async fn by_link(app: &TestApp, path: &str, token: &str) -> Response {
    app.http
        .get(app.url(path))
        .header("x-link-token", token)
        .send()
        .await
        .expect("request")
}

/// Link holders read the threads (anonymous, as in the live room: nobody's name); they write nothing.
#[tokio::test]
async fn link_holders_read_comments_without_names() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let cast = Cast::new(&app, "link").await;
    let thread = start_ok(&app, &cast, &cast.c).await;
    let tid = id(&thread);
    reply_ok(&app, &cast, &cast.b, &tid, "Đã sửa").await;
    let resolved = app
        .request(
            Method::POST,
            &cast.path(&format!("/{tid}/resolve")),
            &cast.b,
        )
        .send()
        .await
        .expect("resolve");
    assert_eq!(resolved.status(), 200);

    let listed = ok_json(by_link(&app, &cast.path(""), &cast.link).await, 200).await;
    assert_eq!(listed["can_resolve"], false);
    let threads = listed["threads"].as_array().expect("threads");
    assert_eq!(threads.len(), 1);
    let seen = &threads[0];
    assert_eq!(seen["id"], tid.as_str());
    assert_eq!(seen["status"], "resolved");
    assert_eq!(seen["anchor"]["offset"], 3);
    assert_eq!(seen["mine"], false);
    assert_eq!(seen["created_by_name"], "");
    assert_eq!(seen["resolved_by_name"], "");
    let comments = seen["comments"].as_array().expect("comments");
    assert_eq!(comments.len(), 2);
    assert_eq!(comments[1]["body"], "Đã sửa");
    assert!(
        comments
            .iter()
            .all(|c| c["author_name"] == "" && c["mine"] == false)
    );
    let text = listed.to_string();
    for hidden in ["Cúc", "Bình", "usr_", "@erpcons.test"] {
        assert!(!text.contains(hidden), "{hidden} in {text}");
    }
    let one = ok_json(
        by_link(&app, &cast.path(&format!("/{tid}")), &cast.link).await,
        200,
    )
    .await;
    assert_eq!(one["comments"][0]["author_name"], "");
    let audited = app
        .count("SELECT count(*) FROM audit_events WHERE action = 'link.comments' AND actor_kind = 'link'")
        .await;
    assert_eq!(audited, 2, "each read is audited as a link view");

    // The link token alone decides, whoever is signed in.
    let signed_in = app
        .request(Method::GET, &cast.path(&format!("/{tid}")), &cast.e)
        .header("x-link-token", &cast.link)
        .send()
        .await
        .expect("request");
    assert_eq!(signed_in.status(), 200);
    let wrong = by_link(&app, &cast.path(""), &"x".repeat(43)).await;
    expect_error(wrong, 404, "NOT_FOUND", false).await;
    let other = upload(&app, &cast.a, "other").await.0;
    let elsewhere = by_link(
        &app,
        &format!("/api/v1/documents/{other}/comments"),
        &cast.link,
    )
    .await;
    expect_error(elsewhere, 404, "NOT_FOUND", false).await;

    // Writes ignore the link token.
    let anonymous_post = app
        .http
        .post(app.url(&cast.path("")))
        .header("x-link-token", &cast.link)
        .header("idempotency-key", new_key())
        .json(&anchor("p1"))
        .send()
        .await
        .expect("request");
    expect_error(anonymous_post, 401, "UNAUTHENTICATED", false).await;
    let posted = app
        .request(Method::POST, &cast.path(""), &cast.e)
        .header("x-link-token", &cast.link)
        .header("idempotency-key", new_key())
        .json(&anchor("p1"))
        .send()
        .await
        .expect("request");
    expect_error(posted, 404, "NOT_FOUND", false).await;

    let viewer = ok_json(app.get_as(&cast.path(""), &cast.d).await, 200).await;
    assert_eq!(viewer["can_resolve"], false);
    assert_eq!(viewer["threads"][0]["created_by_name"], "Cúc Commenter");

    let off = app
        .request(
            Method::DELETE,
            &format!("/api/v1/documents/{}/link", cast.doc),
            &cast.a,
        )
        .send()
        .await
        .expect("disable");
    assert_eq!(off.status(), 204);
    let gone = by_link(&app, &cast.path(""), &cast.link).await;
    expect_error(gone, 404, "NOT_FOUND", false).await;
}

#[tokio::test]
async fn threads_carry_their_comments_and_flags() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let cast = Cast::new(&app, "shape").await;
    let thread = start_ok(&app, &cast, &cast.c).await;
    assert_eq!(thread["status"], "open");
    assert_eq!(thread["region"], "p1");
    assert_eq!(thread["anchor"]["offset"], 3);
    assert_eq!(thread["created_version"], 1);
    assert_eq!(thread["created_by_name"], "Cúc Commenter");
    assert_eq!(thread["mine"], true);
    assert_eq!(thread["document_id"], cast.doc.as_str());
    assert_eq!(thread["resolved_at"], Value::Null);
    let tid = id(&thread);
    let reply = reply_ok(&app, &cast, &cast.b, &tid, "Đã sửa").await;
    assert_eq!(reply["thread_id"], tid.as_str());
    assert_eq!(reply["author_name"], "Bình Editor");
    assert_eq!(reply["mine"], true);
    assert_eq!(reply["edited_at"], Value::Null);

    let seen_by_a = ok_json(app.get_as(&cast.path(""), &cast.a).await, 200).await;
    assert_eq!(seen_by_a["can_resolve"], true);
    let threads = seen_by_a["threads"].as_array().expect("threads");
    assert_eq!(threads.len(), 1);
    assert_eq!(threads[0]["mine"], false);
    let comments = threads[0]["comments"].as_array().expect("comments");
    assert_eq!(comments.len(), 2);
    assert_eq!(comments[0]["body"], "Xem lại đoạn này");
    assert_eq!(comments[1]["body"], "Đã sửa");
    assert!(comments.iter().all(|c| c["mine"] == false));
    let seen_by_c = ok_json(app.get_as(&cast.path(""), &cast.c).await, 200).await;
    assert_eq!(seen_by_c["can_resolve"], false);
    let text = seen_by_a.to_string();
    for hidden in ["usr_", "@erpcons.test"] {
        assert!(!text.contains(hidden), "no actor ids or emails: {text}");
    }

    let early = json!({ "anchor": {}, "version": 2, "body": "x" });
    expect_error(
        start(&app, &cast, &cast.c, &early).await,
        422,
        "INVALID_REQUEST",
        false,
    )
    .await;
}

#[tokio::test]
async fn pages_follow_creation_order_within_a_status() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let cast = Cast::new(&app, "pages").await;
    let mut started = Vec::new();
    for _ in 0..3 {
        started.push(id(&start_ok(&app, &cast, &cast.c).await));
    }
    let mut seen = Vec::new();
    let mut path = cast.path("?limit=2");
    loop {
        let page = ok_json(app.get_as(&path, &cast.b).await, 200).await;
        seen.extend(page["threads"].as_array().expect("threads").iter().map(id));
        match page["next_cursor"].as_str() {
            Some(cursor) => path = cast.path(&format!("?limit=2&cursor={cursor}")),
            None => break,
        }
    }
    assert_eq!(seen, started);
    let bad = app.get_as(&cast.path("?cursor=zz"), &cast.b).await;
    expect_error(bad, 422, "INVALID_REQUEST", false).await;
}

#[tokio::test]
async fn authors_edit_and_delete_only_their_own_words() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let cast = Cast::new(&app, "own").await;
    let thread = start_ok(&app, &cast, &cast.c).await;
    let tid = id(&thread);
    let first = first_comment(&thread);
    let reply = reply_ok(&app, &cast, &cast.a, &tid, "Trả lời của A").await;
    let path = |cid: &str| cast.path(&format!("/{tid}/comments/{cid}"));
    let edit = json!({ "body": "A sửa của C" });
    for who in [&cast.a, &cast.b] {
        let response = app
            .send_json(Method::PATCH, &path(&first), who, &edit)
            .await;
        expect_error(response, 403, "FORBIDDEN", false).await;
        let response = app
            .request(Method::DELETE, &path(&first), who)
            .send()
            .await
            .expect("delete");
        expect_error(response, 403, "FORBIDDEN", false).await;
    }
    let response = app
        .send_json(Method::PATCH, &path(&id(&reply)), &cast.c, &edit)
        .await;
    expect_error(response, 403, "FORBIDDEN", false).await;

    let edited = app
        .send_json(
            Method::PATCH,
            &path(&first),
            &cast.c,
            &json!({ "body": "C sửa" }),
        )
        .await;
    let edited = ok_json(edited, 200).await;
    assert_eq!(edited["body"], "C sửa");
    assert!(edited["edited_at"].is_string());
    let blank = app
        .send_json(
            Method::PATCH,
            &path(&first),
            &cast.c,
            &json!({ "body": "  \n " }),
        )
        .await;
    expect_error(blank, 422, "INVALID_REQUEST", false).await;

    let deleted = app
        .request(Method::DELETE, &path(&first), &cast.c)
        .send()
        .await
        .expect("delete");
    assert_eq!(deleted.status(), 204);
    let again = app
        .request(Method::DELETE, &path(&first), &cast.c)
        .send()
        .await
        .expect("delete");
    expect_error(again, 404, "NOT_FOUND", false).await;
    let edit_gone = app
        .send_json(Method::PATCH, &path(&first), &cast.c, &edit)
        .await;
    expect_error(edit_gone, 404, "NOT_FOUND", false).await;
    let after = ok_json(
        app.get_as(&cast.path(&format!("/{tid}")), &cast.c).await,
        200,
    )
    .await;
    let comments = after["comments"].as_array().expect("comments");
    assert_eq!(comments.len(), 2, "a deleted comment keeps its place");
    assert_eq!(comments[0]["body"], Value::Null);
    assert!(comments[0]["deleted_at"].is_string());
    let stored = app
        .count("SELECT count(*) FROM comments WHERE body = 'C sửa' OR body = 'Xem lại đoạn này'")
        .await;
    assert_eq!(stored, 0, "the words are gone from the database");

    let last = app
        .request(Method::DELETE, &path(&id(&reply)), &cast.a)
        .send()
        .await
        .expect("delete");
    assert_eq!(last.status(), 204);
    let gone = app.get_as(&cast.path(&format!("/{tid}")), &cast.a).await;
    expect_error(gone, 404, "NOT_FOUND", false).await;
    assert_eq!(app.count("SELECT count(*) FROM comment_threads").await, 0);
}

#[tokio::test]
async fn resolving_and_reopening_keep_who_and_when() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let cast = Cast::new(&app, "resolve").await;
    let thread = start_ok(&app, &cast, &cast.c).await;
    let tid = id(&thread);
    let resolve = |who: &str| {
        let path = cast.path(&format!("/{tid}/resolve"));
        let who = who.to_owned();
        let app = &app;
        async move {
            app.request(Method::POST, &path, &who)
                .send()
                .await
                .expect("resolve")
        }
    };
    let resolved = ok_json(resolve(&cast.b).await, 200).await;
    assert_eq!(resolved["status"], "resolved");
    assert_eq!(resolved["resolved_by_name"], "Bình Editor");
    let at = resolved["resolved_at"].clone();
    assert!(at.is_string());
    let again = ok_json(resolve(&cast.a).await, 200).await;
    assert_eq!(
        again["resolved_by_name"], "Bình Editor",
        "resolving twice changes nothing"
    );
    assert_eq!(again["resolved_at"], at);

    let reply = reply_ok(&app, &cast, &cast.c, &tid, "Vẫn còn lỗi").await;
    assert_eq!(reply["thread_id"], tid.as_str());
    let listed = ok_json(
        app.get_as(&cast.path("?status=resolved"), &cast.c).await,
        200,
    )
    .await;
    assert_eq!(
        listed["threads"].as_array().map(Vec::len),
        Some(1),
        "a reply leaves it resolved"
    );
    let open = ok_json(app.get_as(&cast.path("?status=open"), &cast.c).await, 200).await;
    assert_eq!(open["threads"], json!([]));
    expect_error(
        app.get_as(&cast.path("?status=closed"), &cast.c).await,
        422,
        "INVALID_REQUEST",
        false,
    )
    .await;

    let reopen = app
        .request(Method::POST, &cast.path(&format!("/{tid}/reopen")), &cast.a)
        .send()
        .await
        .expect("reopen");
    let reopened = ok_json(reopen, 200).await;
    assert_eq!(reopened["status"], "open");
    assert_eq!(reopened["resolved_by_name"], Value::Null);
    assert_eq!(reopened["resolved_at"], Value::Null);
    let audited = app
        .count(
            "SELECT count(*) FROM audit_events WHERE action IN ('comment.resolve', 'comment.reopen') \
             AND result = 'success'",
        )
        .await;
    assert_eq!(audited, 2, "the no-op resolve is not audited");
}

#[tokio::test]
async fn anchor_batches_move_and_orphan_threads() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let cast = Cast::new(&app, "anchors").await;
    let one = id(&start_ok(&app, &cast, &cast.a).await);
    let two = id(&start_ok(&app, &cast, &cast.c).await);
    let three = id(&start_ok(&app, &cast, &cast.c).await);
    let resolve = app
        .request(
            Method::POST,
            &cast.path(&format!("/{three}/resolve")),
            &cast.a,
        )
        .send()
        .await
        .expect("resolve");
    assert_eq!(resolve.status(), 200);
    let other = Cast::new_for_owner(&app, &cast.a, "other").await;
    let foreign = id(&ok_json(start(&app, &other, &cast.a, &anchor("x")).await, 201).await);

    let batch = json!({ "updates": [
        { "thread_id": one, "anchor": { "region": "p9", "offset": 0 }, "region": "p9" },
        { "thread_id": two, "status": "orphaned", "region": null },
        { "thread_id": three, "status": "orphaned", "anchor": { "moved": true } },
        { "thread_id": foreign, "status": "orphaned" },
    ]});
    let response = app
        .send_json(Method::PATCH, &cast.path("/anchors"), &cast.c, &batch)
        .await;
    assert_eq!(ok_json(response, 200).await["updated"], 3);
    let get = |t: &str| {
        let path = cast.path(&format!("/{t}"));
        let app = &app;
        let token = cast.c.clone();
        async move { ok_json(app.get_as(&path, &token).await, 200).await }
    };
    let moved = get(&one).await;
    assert_eq!(moved["region"], "p9");
    assert_eq!(moved["anchor"], json!({ "region": "p9", "offset": 0 }));
    assert_eq!(moved["status"], "open");
    let orphan = get(&two).await;
    assert_eq!(orphan["status"], "orphaned");
    assert_eq!(orphan["region"], Value::Null);
    assert_eq!(orphan["anchor"]["region"], "p1", "absent fields stay");
    let resolved = get(&three).await;
    assert_eq!(
        resolved["status"], "resolved",
        "a resolved thread stays resolved"
    );
    assert_eq!(resolved["anchor"], json!({ "moved": true }));
    let untouched: String =
        sqlx::query_scalar("SELECT status FROM comment_threads WHERE id = $1::uuid")
            .bind(&foreign)
            .fetch_one(app.pool())
            .await
            .expect("status");
    assert_eq!(untouched, "open", "another document's thread is skipped");
    let orphans = ok_json(
        app.get_as(&cast.path("?status=orphaned"), &cast.b).await,
        200,
    )
    .await;
    assert_eq!(orphans["threads"].as_array().map(Vec::len), Some(1));

    let back = json!({ "updates": [{ "thread_id": two, "status": "open", "region": "p2" }] });
    let response = app
        .send_json(Method::PATCH, &cast.path("/anchors"), &cast.b, &back)
        .await;
    assert_eq!(ok_json(response, 200).await["updated"], 1);
    assert_eq!(get(&two).await["status"], "open");

    let too_many: Vec<Value> = (0..201).map(|_| json!({ "thread_id": one })).collect();
    for bad in [
        json!({ "updates": [] }),
        json!({ "updates": [{ "thread_id": one, "status": "resolved" }] }),
        json!({ "updates": [{ "thread_id": one, "region": "" }] }),
        json!({ "updates": [{ "thread_id": one, "anchor": [1, 2] }] }),
        json!({ "updates": too_many }),
    ] {
        let response = app
            .send_json(Method::PATCH, &cast.path("/anchors"), &cast.c, &bad)
            .await;
        expect_error(response, 422, "INVALID_REQUEST", false).await;
    }
}

impl Cast {
    /// Another document of the same owner, nobody else on it.
    async fn new_for_owner(app: &TestApp, a: &str, title: &str) -> Self {
        let (doc, _) = upload(app, a, title).await;
        Self {
            doc,
            a: a.to_owned(),
            b: String::new(),
            c: String::new(),
            d: String::new(),
            e: String::new(),
            link: String::new(),
            grants: Default::default(),
        }
    }
}

#[tokio::test]
async fn a_revoked_commenter_is_refused_at_the_next_request() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let cast = Cast::new(&app, "revoke").await;
    let thread = start_ok(&app, &cast, &cast.c).await;
    let tid = id(&thread);
    let mine = cast.path(&format!("/{tid}/comments/{}", first_comment(&thread)));

    let demote = json!({ "role": "viewer" });
    let permission = format!(
        "/api/v1/documents/{}/permissions/{}",
        cast.doc, cast.grants[1]
    );
    let response = app
        .send_json(Method::PATCH, &permission, &cast.a, &demote)
        .await;
    assert_eq!(response.status(), 200);
    let read = ok_json(app.get_as(&cast.path(""), &cast.c).await, 200).await;
    assert_eq!(read["can_resolve"], false, "a viewer only reads");
    let edit = app
        .send_json(Method::PATCH, &mine, &cast.c, &json!({ "body": "x" }))
        .await;
    expect_error(edit, 403, "FORBIDDEN", false).await;

    let removed = app
        .request(Method::DELETE, &permission, &cast.a)
        .send()
        .await
        .expect("remove");
    assert_eq!(removed.status(), 204);
    expect_error(
        app.get_as(&cast.path(""), &cast.c).await,
        404,
        "NOT_FOUND",
        false,
    )
    .await;
    let reply = app
        .post_json(
            &cast.path(&format!("/{tid}/replies")),
            &cast.c,
            &json!({ "body": "x" }),
        )
        .await;
    expect_error(reply, 404, "NOT_FOUND", false).await;
    let delete = app
        .request(Method::DELETE, &mine, &cast.c)
        .send()
        .await
        .expect("delete");
    expect_error(delete, 404, "NOT_FOUND", false).await;
    let thread = ok_json(
        app.get_as(&cast.path(&format!("/{tid}")), &cast.a).await,
        200,
    )
    .await;
    assert_eq!(
        thread["comments"][0]["body"], "Xem lại đoạn này",
        "their words stay"
    );
}

#[tokio::test]
async fn a_trashed_document_keeps_its_comments_read_only_for_the_owner() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let cast = Cast::new(&app, "trash").await;
    let thread = start_ok(&app, &cast, &cast.b).await;
    let tid = id(&thread);
    app.trash(&cast.a, &format!("/api/v1/documents/{}", cast.doc))
        .await;
    let listed = ok_json(app.get_as(&cast.path(""), &cast.a).await, 200).await;
    assert_eq!(listed["threads"].as_array().map(Vec::len), Some(1));
    assert_eq!(
        listed["can_resolve"], false,
        "nothing can change in the trash"
    );
    expect_error(
        app.get_as(&cast.path(""), &cast.b).await,
        404,
        "NOT_FOUND",
        false,
    )
    .await;
    for who in [&cast.a, &cast.b] {
        expect_error(
            start(&app, &cast, who, &anchor("p1")).await,
            404,
            "NOT_FOUND",
            false,
        )
        .await;
        let resolve = app
            .request(Method::POST, &cast.path(&format!("/{tid}/resolve")), who)
            .send()
            .await
            .expect("resolve");
        expect_error(resolve, 404, "NOT_FOUND", false).await;
    }
    let reply = app
        .post_json(
            &cast.path(&format!("/{tid}/replies")),
            &cast.a,
            &json!({ "body": "x" }),
        )
        .await;
    expect_error(reply, 404, "NOT_FOUND", false).await;
    let batch = json!({ "updates": [{ "thread_id": tid, "status": "orphaned" }] });
    let anchors = app
        .send_json(Method::PATCH, &cast.path("/anchors"), &cast.a, &batch)
        .await;
    expect_error(anchors, 404, "NOT_FOUND", false).await;
}

#[tokio::test]
async fn bodies_anchors_and_counts_are_bounded() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let cast = Cast::new(&app, "bounds").await;
    let long = "đ".repeat(10_001);
    for body in [
        json!({ "anchor": {}, "body": long }),
        json!({ "anchor": {}, "body": "   " }),
        json!({ "anchor": {}, "body": "a\u{0}b" }),
        json!({ "anchor": {}, "body": "" }),
        json!({ "anchor": { "big": "x".repeat(8200) }, "body": "ok" }),
        json!({ "anchor": [], "body": "ok" }),
        json!({ "body": "ok" }),
        json!({ "anchor": {}, "region": "", "body": "ok" }),
        json!({ "anchor": {}, "region": "r".repeat(257), "body": "ok" }),
        json!({ "anchor": {}, "region": "a\nb", "body": "ok" }),
        json!({ "anchor": {}, "version": 0, "body": "ok" }),
    ] {
        let response = start(&app, &cast, &cast.c, &body).await;
        expect_error(response, 422, "INVALID_REQUEST", false).await;
    }
    let exact = json!({ "anchor": {}, "body": "đ".repeat(10_000) });
    assert_eq!(start(&app, &cast, &cast.c, &exact).await.status(), 201);

    let thread = start_ok(&app, &cast, &cast.c).await;
    let tid = id(&thread);
    sqlx::query(
        "INSERT INTO comments (id, thread_id, author_id, body)
         SELECT gen_random_uuid(), $1::uuid, 'usr_c', 'x' FROM generate_series(1, 199)",
    )
    .bind(&tid)
    .execute(app.pool())
    .await
    .expect("fill thread");
    let full = app
        .post_json(
            &cast.path(&format!("/{tid}/replies")),
            &cast.b,
            &json!({ "body": "x" }),
        )
        .await;
    expect_error(full, 422, "TOO_MANY_COMMENTS", false).await;

    sqlx::query(
        "INSERT INTO comment_threads (id, document_id, anchor, created_version_seq, created_by)
         SELECT gen_random_uuid(), $1::uuid, '{}', 1, 'usr_c' FROM generate_series(1, 1998)",
    )
    .bind(&cast.doc)
    .execute(app.pool())
    .await
    .expect("fill document");
    expect_error(
        start(&app, &cast, &cast.c, &anchor("p1")).await,
        422,
        "TOO_MANY_COMMENTS",
        false,
    )
    .await;
}

#[tokio::test]
async fn creates_replay_by_key_and_writes_are_rate_limited() {
    let Some(app) = TestApp::spawn_with(|s| {
        s.api.comment_rate = office_api::RateConfig {
            burst: 4,
            per_second: 0.001,
        };
    })
    .await
    else {
        return;
    };
    let cast = Cast::new(&app, "keys").await;
    let key = new_key();
    let first = app
        .post_json_keyed(&cast.path(""), &cast.c, &key, &anchor("p1"))
        .await;
    let first = ok_json(first, 201).await;
    let replay = app
        .post_json_keyed(&cast.path(""), &cast.c, &key, &anchor("p1"))
        .await;
    assert_eq!(ok_json(replay, 201).await["id"], first["id"]);
    let other = app
        .post_json_keyed(&cast.path(""), &cast.c, &key, &anchor("p2"))
        .await;
    expect_error(other, 422, "IDEMPOTENCY_MISMATCH", false).await;
    assert_eq!(app.count("SELECT count(*) FROM comment_threads").await, 1);

    let reply = app
        .post_json(
            &cast.path(&format!("/{}/replies", id(&first))),
            &cast.c,
            &json!({ "body": "x" }),
        )
        .await;
    assert_eq!(reply.status(), 201);
    let limited = app
        .post_json(
            &cast.path(&format!("/{}/replies", id(&first))),
            &cast.c,
            &json!({ "body": "y" }),
        )
        .await;
    expect_error(limited, 429, "RATE_LIMITED", true).await;
    let reads = app.get_as(&cast.path(""), &cast.c).await;
    assert_eq!(reads.status(), 200, "reads are not counted");
    let others = start(&app, &cast, &cast.b, &anchor("p3")).await;
    assert_eq!(others.status(), 201, "the limit is per actor");
}

#[tokio::test]
async fn audit_rows_name_the_document_only() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let cast = Cast::new(&app, "audit").await;
    let thread = start_ok(&app, &cast, &cast.c).await;
    let tid = id(&thread);
    reply_ok(&app, &cast, &cast.b, &tid, "Bí mật trả lời").await;
    let resolve = app
        .request(
            Method::POST,
            &cast.path(&format!("/{tid}/resolve")),
            &cast.a,
        )
        .send()
        .await
        .expect("resolve");
    assert_eq!(resolve.status(), 200);
    let gone = app
        .request(Method::DELETE, &cast.path(&format!("/{tid}")), &cast.a)
        .send()
        .await
        .expect("delete");
    assert_eq!(gone.status(), 204);
    let rows: Vec<String> = sqlx::query_scalar(
        "SELECT row_to_json(a)::text FROM audit_events a WHERE action LIKE 'comment.%'",
    )
    .fetch_all(app.pool())
    .await
    .expect("audit");
    let actions: Vec<String> = sqlx::query_scalar(
        "SELECT action FROM audit_events WHERE action LIKE 'comment.%' AND result = 'success' ORDER BY at",
    )
    .fetch_all(app.pool())
    .await
    .expect("actions");
    assert_eq!(
        actions,
        [
            "comment.create",
            "comment.reply",
            "comment.resolve",
            "comment.thread_delete"
        ]
    );
    for row in &rows {
        for hidden in ["Bí mật", "Xem lại", "p1", "Cúc", "Bình", "@erpcons", &tid] {
            assert!(!row.contains(hidden), "{hidden} in {row}");
        }
        assert!(row.contains(&cast.doc), "{row}");
    }
}

#[tokio::test]
async fn a_purge_takes_the_threads_with_it() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let cast = Cast::new(&app, "purge").await;
    let thread = start_ok(&app, &cast, &cast.c).await;
    reply_ok(&app, &cast, &cast.b, &id(&thread), "x").await;
    let keep = Cast::new_for_owner(&app, &cast.a, "keep").await;
    start_ok(&app, &keep, &cast.a).await;
    let path = format!("/api/v1/documents/{}", cast.doc);
    app.trash(&cast.a, &path).await;
    assert_eq!(app.purge(&cast.a, &path, &new_key()).await.status(), 204);
    let left = app
        .count("SELECT count(*) FROM comment_threads WHERE document_id = (SELECT id FROM documents WHERE state = 'PURGED')")
        .await;
    assert_eq!(left, 0);
    assert_eq!(
        app.count("SELECT count(*) FROM comment_threads").await,
        1,
        "other documents keep theirs"
    );
    assert_eq!(app.count("SELECT count(*) FROM comments").await, 1);
}

#[tokio::test]
async fn control_characters_and_oversized_numbers_are_refused() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let cast = Cast::new(&app, "escapes").await;
    let control = json!({ "anchor": {}, "body": "a\u{1}b" });
    expect_error(
        start(&app, &cast, &cast.c, &control).await,
        422,
        "INVALID_REQUEST",
        false,
    )
    .await;
    let lines = json!({ "anchor": {}, "body": "dòng 1\r\ndòng 2\tcột" });
    let thread = ok_json(start(&app, &cast, &cast.c, &lines).await, 201).await;
    assert_eq!(thread["comments"][0]["body"], "dòng 1\r\ndòng 2\tcột");
    let tid = id(&thread);
    let reply = app
        .post_json(
            &cast.path(&format!("/{tid}/replies")),
            &cast.c,
            &json!({ "body": "x\u{7f}" }),
        )
        .await;
    expect_error(reply, 422, "INVALID_REQUEST", false).await;

    let huge: Value =
        serde_json::from_str(r#"{"anchor":{"n":[1e300,-1e300]},"body":"ok"}"#).expect("json");
    expect_error(
        start(&app, &cast, &cast.c, &huge).await,
        422,
        "INVALID_REQUEST",
        false,
    )
    .await;
    let tiny: Value = serde_json::from_str(r#"{"anchor":{"n":1e-300},"body":"ok"}"#).expect("json");
    expect_error(
        start(&app, &cast, &cast.c, &tiny).await,
        422,
        "INVALID_REQUEST",
        false,
    )
    .await;
    let batch: Value = serde_json::from_str(&format!(
        r#"{{"updates":[{{"thread_id":"{tid}","anchor":{{"n":1e300}}}}]}}"#
    ))
    .expect("json");
    let response = app
        .send_json(Method::PATCH, &cast.path("/anchors"), &cast.c, &batch)
        .await;
    expect_error(response, 422, "INVALID_REQUEST", false).await;
    let fine: Value = serde_json::from_str(
        r#"{"anchor":{"offset":12,"big":9007199254740993,"x":0.25,"y":-1500000.5,"z":0},"body":"ok"}"#,
    )
    .expect("json");
    assert_eq!(start(&app, &cast, &cast.c, &fine).await.status(), 201);
}

#[tokio::test]
async fn a_document_holds_a_bounded_amount_of_words() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let cast = Cast::new(&app, "words").await;
    // 20 000 bytes each; the 525th would pass 10 MiB.
    let big = "é".repeat(10_000);
    let mut threads = Vec::new();
    let mut held = 0;
    while held < 524 {
        let body = json!({ "anchor": {}, "body": big });
        let thread = ok_json(start(&app, &cast, &cast.c, &body).await, 201).await;
        let tid = id(&thread);
        threads.push(tid.clone());
        held += 1;
        while held < 524 && held % 175 != 0 {
            reply_ok(&app, &cast, &cast.b, &tid, &big).await;
            held += 1;
        }
    }
    let tid = &threads[0];
    let over = app
        .post_json(
            &cast.path(&format!("/{tid}/replies")),
            &cast.b,
            &json!({ "body": big }),
        )
        .await;
    expect_error(over, 422, "TOO_MANY_COMMENTS", false).await;
    let small = reply_ok(&app, &cast, &cast.b, tid, "x").await;
    let grow = app
        .send_json(
            Method::PATCH,
            &cast.path(&format!("/{tid}/comments/{}", id(&small))),
            &cast.b,
            &json!({ "body": big }),
        )
        .await;
    expect_error(grow, 422, "TOO_MANY_COMMENTS", false).await;
    let started = json!({ "anchor": {}, "body": big });
    expect_error(
        start(&app, &cast, &cast.a, &started).await,
        422,
        "TOO_MANY_COMMENTS",
        false,
    )
    .await;

    let gone = app
        .request(
            Method::DELETE,
            &cast.path(&format!("/{}", threads[1])),
            &cast.a,
        )
        .send()
        .await
        .expect("delete");
    assert_eq!(gone.status(), 204);
    let room = app
        .post_json(
            &cast.path(&format!("/{tid}/replies")),
            &cast.b,
            &json!({ "body": big }),
        )
        .await;
    assert_eq!(room.status(), 201, "deleting frees room");
}

#[tokio::test]
async fn deleted_comments_count_against_a_thread_only_loosely() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let cast = Cast::new(&app, "deleted").await;
    let tid = id(&start_ok(&app, &cast, &cast.c).await);
    let fill = |n: i32| {
        sqlx::query(
            "INSERT INTO comments (id, thread_id, author_id, body, deleted_at)
             SELECT gen_random_uuid(), $1::uuid, 'usr_c', NULL, now() FROM generate_series(1, $2)",
        )
        .bind(tid.clone())
        .bind(n)
        .execute(app.pool())
    };
    fill(199).await.expect("fill");
    let reply = app
        .post_json(
            &cast.path(&format!("/{tid}/replies")),
            &cast.b,
            &json!({ "body": "x" }),
        )
        .await;
    assert_eq!(reply.status(), 201, "deleted comments leave room for words");
    fill(800).await.expect("fill");
    let full = app
        .post_json(
            &cast.path(&format!("/{tid}/replies")),
            &cast.b,
            &json!({ "body": "y" }),
        )
        .await;
    expect_error(full, 422, "TOO_MANY_COMMENTS", false).await;
}
