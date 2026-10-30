//! Sharing (plan v2 §3–4): the role matrix per route, revocation, invitations by email, the owner's
//! quota, the trash and the purge.
use std::time::Duration;

use office_documents::FailPoint;
use reqwest::Method;
use serde_json::{Value, json};

use crate::{
    people::{
        OWNER_EMAIL, enable_link, owner, permission_of, share, share_ok, sign_in, status, upload,
    },
    support::{TestApp, docx, etag, expect_error, ok_json},
};

const B: &str = "b.editor@erpcons.test";
const CM: &str = "cm.commenter@erpcons.test";
const C: &str = "c.viewer@erpcons.test";

/// A's document shared with an editor, a commenter and a viewer; D has no role.
struct Shared {
    app: TestApp,
    doc: String,
    etag: String,
    a: String,
    b: String,
    cm: String,
    c: String,
    d: String,
}

async fn shared() -> Option<Shared> {
    let app = TestApp::spawn().await?;
    let a = owner(&app).await;
    let b = sign_in(&app, "usr_b", "Bình Biên Tập", B, true).await;
    let cm = sign_in(&app, "usr_cm", "Cúc Góp Ý", CM, true).await;
    let c = sign_in(&app, "usr_c", "Chi Xem", C, true).await;
    let d = sign_in(&app, "usr_d", "Dũng Lạ", "d.stranger@erpcons.test", true).await;
    let (doc, etag) = upload(&app, &a, "shared-doc").await;
    share_ok(&app, &a, &doc, &[B], "editor").await;
    share_ok(&app, &a, &doc, &[CM], "commenter").await;
    let body = share_ok(&app, &a, &doc, &[C], "viewer").await;
    assert_eq!(
        body["permissions"].as_array().map(Vec::len),
        Some(3),
        "{body}"
    );
    Some(Shared {
        app,
        doc,
        etag,
        a,
        b,
        cm,
        c,
        d,
    })
}

impl Shared {
    fn path(&self, rest: &str) -> String {
        format!("/api/v1/documents/{}{rest}", self.doc)
    }

    async fn permission_list(&self) -> Value {
        ok_json(
            self.app.get_as(&self.path("/permissions"), &self.a).await,
            200,
        )
        .await
    }

    /// Statuses of the same request by A, B, the commenter, C and D.
    async fn statuses(&self, method: Method, rest: &str, body: Option<&Value>) -> [u16; 5] {
        let mut out = [0; 5];
        for (i, token) in [&self.a, &self.b, &self.cm, &self.c, &self.d]
            .into_iter()
            .enumerate()
        {
            let mut request = self.app.request(method.clone(), &self.path(rest), token);
            if let Some(body) = body {
                request = request.json(body);
            }
            out[i] = status(request.send().await.expect("request")).await;
        }
        out
    }

    /// Statuses of the commenter, C and D, then of B; A is left out where its success would change
    /// what the others see.
    async fn others(&self, method: Method, rest: &str, body: Option<&Value>) -> [u16; 4] {
        let mut out = [0; 4];
        for (i, token) in [&self.cm, &self.c, &self.d, &self.b]
            .into_iter()
            .enumerate()
        {
            let mut request = self.app.request(method.clone(), &self.path(rest), token);
            if let Some(body) = body {
                request = request.json(body);
            }
            out[i] = status(request.send().await.expect("request")).await;
        }
        out
    }
}

#[tokio::test]
async fn reading_follows_the_role_matrix() {
    let Some(s) = shared().await else {
        return;
    };
    assert_eq!(
        s.statuses(Method::GET, "", None).await,
        [200, 200, 200, 200, 404]
    );
    assert_eq!(
        s.statuses(Method::GET, "/content", None).await,
        [200, 200, 200, 200, 404]
    );
    assert_eq!(
        s.statuses(Method::GET, "/versions", None).await,
        [200, 200, 403, 403, 404]
    );
    assert_eq!(
        s.statuses(Method::GET, "/versions/1/content", None).await,
        [200, 200, 403, 403, 404]
    );
    assert_eq!(
        s.statuses(Method::GET, "/permissions", None).await,
        [200, 200, 200, 200, 404]
    );
    for (token, role, folder) in [
        (&s.a, "owner", true),
        (&s.b, "editor", false),
        (&s.cm, "commenter", false),
        (&s.c, "viewer", false),
    ] {
        let doc = ok_json(s.app.get_as(&s.path(""), token).await, 200).await;
        assert_eq!(doc["role"], role, "{doc}");
        assert_eq!(doc["owner_name"], office_identity::testing::DEFAULT_NAME);
        assert_eq!(doc["shared"], true);
        assert_eq!(doc["can_download"], true);
        assert!(doc.get("folder_id").is_some());
        if !folder {
            assert_eq!(doc["folder_id"], Value::Null);
        }
    }
    // A forbidden answer is the contract's error body.
    expect_error(
        s.app.get_as(&s.path("/versions"), &s.c).await,
        403,
        "FORBIDDEN",
        false,
    )
    .await;
}

#[tokio::test]
async fn the_share_dialog_shows_viewers_only_their_own_permission() {
    let Some(s) = shared().await else {
        return;
    };
    for (token, seen, can_share) in [
        (&s.a, 3, true),
        (&s.b, 3, true),
        (&s.cm, 1, false),
        (&s.c, 1, false),
    ] {
        let body = ok_json(s.app.get_as(&s.path("/permissions"), token).await, 200).await;
        assert_eq!(
            body["permissions"].as_array().map(Vec::len),
            Some(seen),
            "{body}"
        );
        assert_eq!(body["can_share"], can_share, "{body}");
        assert_eq!(body["owner"]["email"], OWNER_EMAIL);
        assert_eq!(
            body["settings"],
            json!({ "editors_can_share": true, "viewers_can_download": true })
        );
        assert_eq!(
            body["link"],
            json!({ "enabled": false, "role": null, "expires_at": null })
        );
    }
    let own = ok_json(s.app.get_as(&s.path("/permissions"), &s.c).await, 200).await;
    assert_eq!(own["permissions"][0]["email"], C);
    assert_eq!(own["permissions"][0]["role"], "viewer");
    assert_eq!(own["permissions"][0]["pending"], false);
    assert_eq!(own["role"], "viewer");
}

#[tokio::test]
async fn writing_follows_the_role_matrix() {
    let Some(s) = shared().await else {
        return;
    };
    // Saves: the commenter and the viewer may not, D sees nothing, B may.
    let mut save = [0; 4];
    for (i, token) in [&s.cm, &s.c, &s.d, &s.b].into_iter().enumerate() {
        let response = s
            .app
            .put_content(token, &s.doc, Some(&s.etag), docx("by-someone"))
            .await;
        save[i] = status(response).await;
    }
    assert_eq!(save, [403, 403, 404, 200]);
    let current = s.app.get_as(&s.path(""), &s.a).await;
    let tag = etag(&current);
    let restore = |token: &String| {
        s.app
            .request(Method::POST, &s.path("/versions/1/restore"), token)
            .header("if-match", tag.clone())
            .send()
    };
    let mut restored = [0; 4];
    for (i, token) in [&s.cm, &s.c, &s.d, &s.b].into_iter().enumerate() {
        restored[i] = status(restore(token).await.expect("restore")).await;
    }
    assert_eq!(restored, [403, 403, 404, 200]);

    let rename = json!({ "title": "Đổi tên bởi người được chia sẻ" });
    assert_eq!(
        s.others(Method::PATCH, "", Some(&rename)).await,
        [403, 403, 404, 200]
    );
    let doc = ok_json(s.app.get_as(&s.path(""), &s.a).await, 200).await;
    assert_eq!(doc["title"], "Đổi tên bởi người được chia sẻ");
    // Folders are the owner's: nobody else moves the document, not even an editor.
    let move_to_root = json!({ "folder_id": null });
    assert_eq!(
        s.others(Method::PATCH, "", Some(&move_to_root)).await,
        [403, 403, 404, 403]
    );
    let settings = json!({ "editors_can_share": false, "viewers_can_download": false });
    assert_eq!(
        s.others(Method::PUT, "/share-settings", Some(&settings))
            .await,
        [403, 403, 404, 403]
    );
    assert_eq!(
        s.others(Method::DELETE, "", None).await,
        [403, 403, 404, 403]
    );
    assert_eq!(
        s.others(Method::POST, "/restore", None).await,
        [403, 403, 404, 403]
    );
    let mut purge = [0; 4];
    for (i, token) in [&s.cm, &s.c, &s.d, &s.b].into_iter().enumerate() {
        purge[i] = status(
            s.app
                .purge(token, &s.path(""), &crate::support::new_key())
                .await,
        )
        .await;
    }
    assert_eq!(purge, [403, 403, 404, 403]);
    // The owner still may do all of it.
    assert_eq!(
        status(
            s.app
                .send_json(Method::PUT, &s.path("/share-settings"), &s.a, &settings)
                .await
        )
        .await,
        200
    );
}

#[tokio::test]
async fn sharing_follows_the_role_matrix_and_the_editors_switch() {
    let Some(s) = shared().await else {
        return;
    };
    let new = sign_in(&s.app, "usr_e", "Én Mới", "e.new@erpcons.test", true).await;
    let add = json!({ "emails": ["e.new@erpcons.test"], "role": "viewer", "notify": false });
    assert_eq!(
        s.others(Method::POST, "/permissions", Some(&add)).await,
        [403, 403, 404, 200]
    );
    assert_eq!(s.app.get_as(&s.path(""), &new).await.status(), 200);
    assert_eq!(
        s.others(Method::PUT, "/link", None).await,
        [403, 403, 404, 200]
    );
    assert_eq!(
        s.others(Method::DELETE, "/link", None).await,
        [403, 403, 404, 204]
    );

    let off = json!({ "editors_can_share": false, "viewers_can_download": true });
    ok_json(
        s.app
            .send_json(Method::PUT, &s.path("/share-settings"), &s.a, &off)
            .await,
        200,
    )
    .await;
    assert_eq!(
        s.others(Method::POST, "/permissions", Some(&add)).await,
        [403, 403, 404, 403]
    );
    assert_eq!(
        s.others(Method::PUT, "/link", None).await,
        [403, 403, 404, 403]
    );
    let dialog = ok_json(s.app.get_as(&s.path("/permissions"), &s.b).await, 200).await;
    assert_eq!(dialog["can_share"], false);
    // An editor who may not share may still leave.
    let own = permission_of(&dialog, B);
    let leave = s
        .app
        .request(
            Method::DELETE,
            &s.path(&format!("/permissions/{own}")),
            &s.b,
        )
        .send()
        .await
        .expect("leave");
    assert_eq!(leave.status(), 204);
    assert_eq!(s.app.get_as(&s.path(""), &s.b).await.status(), 404);
}

impl Shared {
    async fn patch_permission(&self, token: &str, permission: &str, body: Value) -> u16 {
        status(
            self.app
                .send_json(
                    Method::PATCH,
                    &self.path(&format!("/permissions/{permission}")),
                    token,
                    &body,
                )
                .await,
        )
        .await
    }

    /// The permission of `email` as the owner's dialog shows it.
    async fn permission(&self, email: &str) -> Value {
        let dialog = ok_json(
            self.app.get_as(&self.path("/permissions"), &self.a).await,
            200,
        )
        .await;
        dialog["permissions"]
            .as_array()
            .expect("permissions")
            .iter()
            .find(|p| p["email"] == email)
            .cloned()
            .unwrap_or(Value::Null)
    }

    async fn add(&self, token: &str, emails: &[&str], role: &str) -> u16 {
        status(share(&self.app, token, &self.doc, emails, role, false).await).await
    }
}

#[tokio::test]
async fn nobody_changes_their_own_permission_and_only_the_owner_sets_expiries() {
    let Some(s) = shared().await else {
        return;
    };
    let b = permission_of(&s.permission_list().await, B);
    let c = permission_of(&s.permission_list().await, C);
    let cm = permission_of(&s.permission_list().await, CM);
    let until = json!({ "expires_at": "2999-01-01T00:00:00Z" });
    assert_eq!(s.patch_permission(&s.a, &b, until.clone()).await, 200);
    assert_eq!(s.patch_permission(&s.a, &c, until.clone()).await, 200);

    // B may share, but not touch its own grant: no lifting the owner's expiry, no role change.
    assert_eq!(
        s.patch_permission(&s.b, &b, json!({ "expires_at": null }))
            .await,
        403
    );
    assert_eq!(
        s.patch_permission(&s.b, &b, json!({ "role": "viewer" }))
            .await,
        403
    );
    // Nor any expiry of other people's.
    for body in [
        json!({ "expires_at": null }),
        json!({ "expires_at": "2998-01-01T00:00:00Z" }),
        json!({ "role": "commenter", "expires_at": null }),
    ] {
        assert_eq!(s.patch_permission(&s.b, &c, body).await, 403);
    }
    assert_eq!(
        s.patch_permission(&s.b, &cm, json!({ "expires_at": "2998-01-01T00:00:00Z" }))
            .await,
        403
    );
    // Roles of others, yes; the owner's expiry stays.
    assert_eq!(
        s.patch_permission(&s.b, &c, json!({ "role": "commenter" }))
            .await,
        200
    );
    let b_now = s.permission(B).await;
    assert_eq!(b_now["role"], "editor");
    assert_eq!(b_now["expires_at"], "2999-01-01T00:00:00Z");
    let c_now = s.permission(C).await;
    assert_eq!(c_now["role"], "commenter");
    assert_eq!(c_now["expires_at"], "2999-01-01T00:00:00Z");
    // The commenter has no sharing rights at all, its own grant included.
    assert_eq!(
        s.patch_permission(&s.cm, &cm, json!({ "role": "editor" }))
            .await,
        403
    );
    // The owner sets and clears expiries.
    assert_eq!(
        s.patch_permission(&s.a, &c, json!({ "expires_at": null }))
            .await,
        200
    );
    assert_eq!(s.permission(C).await["expires_at"], Value::Null);
    // Leaving stays open to everyone, a time-limited grant included.
    let leave = s
        .app
        .request(Method::DELETE, &s.path(&format!("/permissions/{b}")), &s.b)
        .send()
        .await
        .expect("leave");
    assert_eq!(leave.status(), 204);
}

#[tokio::test]
async fn only_the_owner_removes_a_time_limited_permission_of_someone_else() {
    let Some(s) = shared().await else {
        return;
    };
    const PENDING: &str = "p.pending@erpcons.test";
    assert_eq!(s.add(&s.a, &[PENDING], "viewer").await, 200);
    let list = s.permission_list().await;
    let (c, cm, pending) = (
        permission_of(&list, C),
        permission_of(&list, CM),
        permission_of(&list, PENDING),
    );
    let until = json!({ "expires_at": "2999-01-01T00:00:00Z" });
    assert_eq!(s.patch_permission(&s.a, &c, until.clone()).await, 200);
    assert_eq!(s.patch_permission(&s.a, &pending, until).await, 200);
    let remove = |token: &String, permission: &String| {
        let request = s.app.request(
            Method::DELETE,
            &s.path(&format!("/permissions/{permission}")),
            token,
        );
        async move { status(request.send().await.expect("remove")).await }
    };

    // Removing and inviting again would lift the owner's expiry.
    assert_eq!(remove(&s.b, &c).await, 403);
    assert_eq!(remove(&s.b, &pending).await, 403);
    assert_eq!(s.app.get_as(&s.path(""), &s.c).await.status(), 200);
    // A permission without expiry, the editor may remove.
    assert_eq!(remove(&s.b, &cm).await, 204);
    // The owner may remove either.
    assert_eq!(remove(&s.a, &c).await, 204);
    assert_eq!(remove(&s.a, &pending).await, 204);
    assert_eq!(s.app.get_as(&s.path(""), &s.c).await.status(), 404);
}

#[tokio::test]
async fn sharing_again_keeps_the_owners_expiry_and_never_regrants_oneself() {
    let Some(s) = shared().await else {
        return;
    };
    let b = permission_of(&s.permission_list().await, B);
    let c = permission_of(&s.permission_list().await, C);
    let until = json!({ "expires_at": "2999-01-01T00:00:00Z" });
    assert_eq!(s.patch_permission(&s.a, &b, until.clone()).await, 200);
    assert_eq!(s.patch_permission(&s.a, &c, until).await, 200);

    // An editor's re-invitation changes the role and keeps the expiry.
    assert_eq!(s.add(&s.b, &[C], "commenter").await, 200);
    let c_now = s.permission(C).await;
    assert_eq!(c_now["role"], "commenter");
    assert_eq!(c_now["expires_at"], "2999-01-01T00:00:00Z");
    // Inviting oneself changes nothing.
    assert_eq!(s.add(&s.b, &[B], "viewer").await, 200);
    let b_now = s.permission(B).await;
    assert_eq!(b_now["role"], "editor");
    assert_eq!(b_now["expires_at"], "2999-01-01T00:00:00Z");
    // No role above editor, for anyone.
    for role in ["owner", "link"] {
        assert_eq!(s.add(&s.b, &["x@erpcons.test"], role).await, 422);
        assert_eq!(
            s.patch_permission(&s.b, &c, json!({ "role": role })).await,
            422
        );
    }

    // An expired permission is the owner's to grant again.
    sqlx::query(
        "UPDATE document_acl SET expires_at = now() - interval '1 second' WHERE id = $1::uuid",
    )
    .bind(&c)
    .execute(s.app.pool())
    .await
    .expect("expire");
    assert_eq!(s.add(&s.b, &[C], "viewer").await, 403);
    assert_eq!(s.app.get_as(&s.path(""), &s.c).await.status(), 404);
    assert_eq!(s.add(&s.a, &[C], "viewer").await, 200);
    assert_eq!(s.app.get_as(&s.path(""), &s.c).await.status(), 200);
    assert_eq!(s.permission(C).await["expires_at"], Value::Null);
}

#[tokio::test]
async fn a_share_colliding_once_is_retried_and_twice_asks_for_a_resend() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let a = owner(&app).await;
    let (doc, _) = upload(&app, &a, "collide").await;
    let points = app.documents().fail_points();
    points.arm_times(FailPoint::ShareCollision, 1);
    let body = share_ok(&app, &a, &doc, &["r1@erpcons.test"], "viewer").await;
    assert_eq!(
        body["permissions"].as_array().map(Vec::len),
        Some(1),
        "{body}"
    );

    points.arm_times(FailPoint::ShareCollision, 2);
    let response = share(
        &app,
        &a,
        &doc,
        &["r1@erpcons.test", "r2@erpcons.test"],
        "editor",
        false,
    )
    .await;
    assert_eq!(response.headers()["retry-after"], "1");
    expect_error(response, 503, "UNAVAILABLE", true).await;
    // Nothing of the request was kept.
    let dialog = ok_json(
        app.get_as(&format!("/api/v1/documents/{doc}/permissions"), &a)
            .await,
        200,
    )
    .await;
    let permissions = dialog["permissions"].as_array().expect("permissions");
    assert_eq!(permissions.len(), 1, "{dialog}");
    assert_eq!(permissions[0]["role"], "viewer");
    // Sending it again works.
    let body = share_ok(
        &app,
        &a,
        &doc,
        &["r1@erpcons.test", "r2@erpcons.test"],
        "editor",
    )
    .await;
    assert_eq!(
        body["permissions"].as_array().map(Vec::len),
        Some(2),
        "{body}"
    );
}

#[tokio::test]
async fn others_cannot_download_when_the_owner_says_so() {
    let Some(s) = shared().await else {
        return;
    };
    let off = json!({ "editors_can_share": true, "viewers_can_download": false });
    ok_json(
        s.app
            .send_json(Method::PUT, &s.path("/share-settings"), &s.a, &off)
            .await,
        200,
    )
    .await;
    for (token, can) in [(&s.a, true), (&s.b, true), (&s.cm, false), (&s.c, false)] {
        let doc = ok_json(s.app.get_as(&s.path(""), token).await, 200).await;
        assert_eq!(doc["can_download"], can, "{doc}");
    }
}

#[tokio::test]
async fn a_removed_permission_is_refused_at_the_next_request() {
    let Some(s) = shared().await else {
        return;
    };
    assert_eq!(s.app.get_as(&s.path(""), &s.b).await.status(), 200);
    let dialog = ok_json(s.app.get_as(&s.path("/permissions"), &s.a).await, 200).await;
    let b = permission_of(&dialog, B);
    let c = permission_of(&dialog, C);
    // A viewer may not remove someone else's permission, and learns nothing about unknown ids.
    for id in [b.clone(), uuid::Uuid::now_v7().to_string()] {
        let response = s
            .app
            .request(Method::DELETE, &s.path(&format!("/permissions/{id}")), &s.c)
            .send()
            .await
            .expect("remove");
        assert_eq!(response.status(), 403);
    }
    let removed = s
        .app
        .request(Method::DELETE, &s.path(&format!("/permissions/{b}")), &s.a)
        .send()
        .await
        .expect("remove");
    assert_eq!(removed.status(), 204);
    // Same token, still cached as verified: the database decides.
    assert_eq!(s.app.get_as(&s.path(""), &s.b).await.status(), 404);
    let save = s
        .app
        .put_content(&s.b, &s.doc, Some(&s.etag), docx("after-removal"))
        .await;
    assert_eq!(save.status(), 404);
    let again = s
        .app
        .request(Method::DELETE, &s.path(&format!("/permissions/{b}")), &s.a)
        .send()
        .await
        .expect("remove again");
    assert_eq!(again.status(), 404);

    // A role change applies at once too.
    let patched = ok_json(
        s.app
            .send_json(
                Method::PATCH,
                &s.path(&format!("/permissions/{c}")),
                &s.a,
                &json!({ "role": "editor" }),
            )
            .await,
        200,
    )
    .await;
    assert_eq!(patched["role"], "editor");
    assert_eq!(
        status(s.app.get_as(&s.path("/versions"), &s.c).await).await,
        200
    );
    // An expiry in the past is refused; one in the future is kept.
    let past = s
        .app
        .send_json(
            Method::PATCH,
            &s.path(&format!("/permissions/{c}")),
            &s.a,
            &json!({ "expires_at": "2020-01-01T00:00:00Z" }),
        )
        .await;
    expect_error(past, 422, "INVALID_REQUEST", false).await;
    let future = ok_json(
        s.app
            .send_json(
                Method::PATCH,
                &s.path(&format!("/permissions/{c}")),
                &s.a,
                &json!({ "expires_at": "2999-01-01T00:00:00Z" }),
            )
            .await,
        200,
    )
    .await;
    assert_eq!(future["expires_at"], "2999-01-01T00:00:00Z");
    // An expired permission counts as removed.
    sqlx::query(
        "UPDATE document_acl SET expires_at = now() - interval '1 second' WHERE id = $1::uuid",
    )
    .bind(&c)
    .execute(s.app.pool())
    .await
    .expect("expire");
    assert_eq!(s.app.get_as(&s.path(""), &s.c).await.status(), 404);
}

#[tokio::test]
async fn invitations_wait_for_a_verified_matching_email() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let a = owner(&app).await;
    let (doc, _) = upload(&app, &a, "invite").await;
    let body = share_ok(&app, &a, &doc, &["  Ế.Mời@ERPCons.test "], "commenter").await;
    let invite = &body["permissions"][0];
    assert_eq!(invite["pending"], true, "{body}");
    assert_eq!(invite["actor_id"], Value::Null);
    assert_eq!(invite["email"], "ế.mời@erpcons.test");

    // The same address, unverified at PI: no access, the invitation stays pending.
    let impostor = sign_in(&app, "usr_f", "Giả Mạo", "ế.mời@erpcons.test", false).await;
    let path = format!("/api/v1/documents/{doc}");
    assert_eq!(app.get_as(&path, &impostor).await.status(), 404);
    // Another verified address gets nothing either.
    let other = sign_in(&app, "usr_g", "Khác", "khac@erpcons.test", true).await;
    assert_eq!(app.get_as(&path, &other).await.status(), 404);
    assert_eq!(
        app.count(
            "SELECT count(*) FROM document_acl WHERE actor_id IS NULL AND revoked_at IS NULL"
        )
        .await,
        1
    );

    // PI reports the address verified (NFC and case differ): the invitation becomes a permission.
    let nfd = "E\u{0302}\u{0301}.Mo\u{031b}\u{0300}i@erpcons.test";
    let invited = sign_in(&app, "usr_e", "Ế Được Mời", nfd, true).await;
    let opened = ok_json(app.get_as(&path, &invited).await, 200).await;
    assert_eq!(opened["role"], "commenter");
    let dialog = ok_json(app.get_as(&format!("{path}/permissions"), &a).await, 200).await;
    assert_eq!(dialog["permissions"][0]["pending"], false, "{dialog}");
    assert_eq!(dialog["permissions"][0]["actor_id"], "usr_e");
    assert_eq!(app.get_as(&path, &impostor).await.status(), 404);

    // The owner's and the caller's own addresses are skipped; a repeat changes nothing.
    let again = share_ok(
        &app,
        &a,
        &doc,
        &[OWNER_EMAIL, "ế.mời@erpcons.test"],
        "commenter",
    )
    .await;
    assert_eq!(
        again["permissions"].as_array().map(Vec::len),
        Some(1),
        "{again}"
    );
}

#[tokio::test]
async fn bad_share_requests_are_refused() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let a = owner(&app).await;
    let (doc, _) = upload(&app, &a, "bad-share").await;
    for (emails, role) in [
        (json!([]), "viewer"),
        (json!(["not-an-email"]), "viewer"),
        (json!(["x@erpcons.test"]), "owner"),
        (json!(["x@erpcons.test"]), "link"),
        (json!(vec!["x@erpcons.test"; 51]), "viewer"),
    ] {
        let response = app
            .send_json(
                Method::POST,
                &format!("/api/v1/documents/{doc}/permissions"),
                &a,
                &json!({ "emails": emails, "role": role, "notify": false }),
            )
            .await;
        expect_error(response, 422, "INVALID_REQUEST", false).await;
    }
    let control = share(&app, &a, &doc, &["x@erpcons.test"], "viewer", true).await;
    assert_eq!(control.status(), 200);
    let bad_message = app
        .send_json(
            Method::POST,
            &format!("/api/v1/documents/{doc}/permissions"),
            &a,
            &json!({ "emails": ["y@erpcons.test"], "role": "viewer", "notify": true, "message": "a\u{7}b" }),
        )
        .await;
    expect_error(bad_message, 422, "INVALID_REQUEST", false).await;
    // Without SMTP nothing is queued.
    assert_eq!(app.count("SELECT count(*) FROM mail_outbox").await, 0);
}

#[tokio::test]
async fn sharing_with_notify_queues_one_mail_per_change() {
    let Some(app) = TestApp::spawn_with(|s| {
        s.api.mail = office_api::MailPolicy {
            enabled: true,
            ..office_api::MailPolicy::default()
        };
    })
    .await
    else {
        return;
    };
    let a = owner(&app).await;
    let (doc, _) = upload(&app, &a, "Bí mật tiêu đề 77aa").await;
    let send = |emails: Vec<&'static str>, role: &'static str| {
        let (app, a, doc) = (&app, &a, &doc);
        async move {
            let response = app
                .send_json(
                    Method::POST,
                    &format!("/api/v1/documents/{doc}/permissions"),
                    a,
                    &json!({ "emails": emails, "role": role, "notify": true, "message": "Nhờ anh xem" }),
                )
                .await;
            assert_eq!(ok_json(response, 200).await["mail_skipped"], 0);
        }
    };
    send(vec!["m1@erpcons.test", "m2@erpcons.test"], "viewer").await;
    assert_eq!(app.count("SELECT count(*) FROM mail_outbox").await, 2);
    send(vec!["m1@erpcons.test"], "viewer").await;
    assert_eq!(
        app.count("SELECT count(*) FROM mail_outbox").await,
        2,
        "no change, no mail"
    );
    send(vec!["m1@erpcons.test"], "editor").await;
    assert_eq!(app.count("SELECT count(*) FROM mail_outbox").await, 3);
    let payloads: Vec<String> = sqlx::query_scalar("SELECT payload::text FROM mail_outbox")
        .fetch_all(app.pool())
        .await
        .expect("payloads");
    for payload in &payloads {
        assert!(
            !payload.contains("Bí mật tiêu đề"),
            "title only when configured: {payload}"
        );
        assert!(payload.contains("Nhờ anh xem"), "{payload}");
    }
}

#[tokio::test]
async fn invite_mails_stop_at_the_daily_cap_but_sharing_does_not() {
    let Some(app) = TestApp::spawn_with(|s| {
        s.api.mail = office_api::MailPolicy {
            enabled: true,
            include_title: false,
            daily_cap: 2,
            ..office_api::MailPolicy::default()
        };
    })
    .await
    else {
        return;
    };
    let a = owner(&app).await;
    let b = sign_in(&app, "usr_b", "Bình", B, true).await;
    let (doc, _) = upload(&app, &a, "capped").await;
    let send = |token: &String, emails: Vec<&'static str>, notify: bool| {
        let (app, doc, token) = (&app, &doc, token.clone());
        async move {
            let response = app
                .send_json(
                    Method::POST,
                    &format!("/api/v1/documents/{doc}/permissions"),
                    &token,
                    &json!({ "emails": emails, "role": "editor", "notify": notify }),
                )
                .await;
            ok_json(response, 200).await
        }
    };
    let outbox = || app.count("SELECT count(*) FROM mail_outbox");

    let body = send(&a, vec![B, "k1@erpcons.test", "k2@erpcons.test"], true).await;
    assert_eq!(body["mail_skipped"], 1, "{body}");
    assert_eq!(body["permissions"].as_array().map(Vec::len), Some(3));
    assert_eq!(outbox().await, 2);
    // Over the cap: still shared, still in the recipient's list, no mail.
    let body = send(&a, vec!["k3@erpcons.test"], true).await;
    assert_eq!(body["mail_skipped"], 1);
    assert_eq!(body["permissions"].as_array().map(Vec::len), Some(4));
    assert_eq!(outbox().await, 2);
    let body = send(&a, vec!["k4@erpcons.test"], false).await;
    assert_eq!(body["mail_skipped"], 0, "no mail asked, none skipped");
    let listed = ok_json(app.get_as("/api/v1/shared", &b).await, 200).await;
    assert_eq!(
        listed["documents"].as_array().map(Vec::len),
        Some(1),
        "{listed}"
    );
    // The cap is per sender: the editor has its own.
    let body = send(&b, vec!["k5@erpcons.test"], true).await;
    assert_eq!(body["mail_skipped"], 0);
    assert_eq!(outbox().await, 3);
    let senders = app
        .count("SELECT count(DISTINCT sender_id) FROM mail_outbox WHERE sender_id IS NOT NULL")
        .await;
    assert_eq!(senders, 2);
    // Rows older than the rolling window no longer count.
    sqlx::query("UPDATE mail_outbox SET created_at = now() - interval '25 hours'")
        .execute(app.pool())
        .await
        .expect("age");
    let body = send(&a, vec!["k6@erpcons.test"], true).await;
    assert_eq!(body["mail_skipped"], 0);
}

#[tokio::test]
async fn an_email_pi_stops_vouching_for_is_forgotten_but_access_stays() {
    let Some(app) = TestApp::spawn_with(|s| {
        s.identity.positive_ttl = Duration::ZERO;
        s.identity.name_ttl = Duration::ZERO;
    })
    .await
    else {
        return;
    };
    let a = owner(&app).await;
    let (doc, _) = upload(&app, &a, "kept").await;
    share_ok(&app, &a, &doc, &["e.cu@erpcons.test"], "viewer").await;
    let e = sign_in(&app, "usr_e", "Ế", "e.cu@erpcons.test", true).await;
    let path = format!("/api/v1/documents/{doc}");
    assert_eq!(app.get_as(&path, &e).await.status(), 200);
    let email_of = || {
        sqlx::query_scalar::<_, String>("SELECT email_norm FROM actor_emails WHERE actor_id = $1")
            .bind("usr_e")
            .fetch_optional(app.pool())
    };
    assert_eq!(
        email_of().await.expect("email").as_deref(),
        Some("e.cu@erpcons.test")
    );

    // PI no longer reports the address verified: it is forgotten, the permission stays.
    app.pi.set_email("usr_e", "e.cu@erpcons.test", false);
    assert_eq!(app.get_as(&path, &e).await.status(), 200);
    assert_eq!(email_of().await.expect("email"), None);
    let (doc2, _) = upload(&app, &a, "kept-2").await;
    let body = share_ok(&app, &a, &doc2, &["e.cu@erpcons.test"], "viewer").await;
    assert_eq!(body["permissions"][0]["pending"], true, "{body}");
    assert_eq!(
        app.get_as(&format!("/api/v1/documents/{doc2}"), &e)
            .await
            .status(),
        404
    );

    // A different verified address replaces the record and takes its own invitations.
    app.pi.set_email("usr_e", "e.moi@erpcons.test", true);
    assert_eq!(app.get_as(&path, &e).await.status(), 200);
    assert_eq!(
        email_of().await.expect("email").as_deref(),
        Some("e.moi@erpcons.test")
    );
    let (doc3, _) = upload(&app, &a, "kept-3").await;
    let body = share_ok(&app, &a, &doc3, &["e.moi@erpcons.test"], "viewer").await;
    assert_eq!(body["permissions"][0]["pending"], false, "{body}");
    assert_eq!(body["permissions"][0]["actor_id"], "usr_e");
}

#[tokio::test]
async fn an_editors_save_counts_against_the_owners_quota() {
    let Some(s) = shared().await else {
        return;
    };
    let used = |token: &String| {
        let app = &s.app;
        let token = token.clone();
        async move {
            let me = ok_json(app.get_as("/api/v1/me", &token).await, 200).await;
            me["quota"]["used_bytes"].as_u64().expect("used")
        }
    };
    let before = used(&s.a).await;
    let bytes = docx("editor-save-quota-1234567890");
    let size = bytes.len() as u64;
    let saved = s.app.put_content(&s.b, &s.doc, Some(&s.etag), bytes).await;
    assert_eq!(saved.status(), 200);
    assert_eq!(used(&s.a).await, before + size);
    assert_eq!(used(&s.b).await, 0);
    let author: String = sqlx::query_scalar(
        "SELECT author_id FROM document_versions WHERE document_id = $1::uuid ORDER BY seq DESC LIMIT 1",
    )
    .bind(&s.doc)
    .fetch_one(s.app.pool())
    .await
    .expect("author");
    assert_eq!(author, "usr_b");
}

#[tokio::test]
async fn the_owners_trash_hides_the_document_from_everyone_else() {
    let Some(s) = shared().await else {
        return;
    };
    let link = enable_link(&s.app, &s.a, &s.doc).await;
    let listed = |token: &String| {
        let app = &s.app;
        let token = token.clone();
        async move {
            let body = ok_json(app.get_as("/api/v1/shared", &token).await, 200).await;
            body["documents"].as_array().map_or(0, Vec::len)
        }
    };
    assert_eq!(listed(&s.b).await, 1);
    s.app.trash(&s.a, &s.path("")).await;
    assert_eq!(
        s.statuses(Method::GET, "", None).await,
        [200, 404, 404, 404, 404]
    );
    assert_eq!(listed(&s.b).await, 0);
    assert_eq!(s.app.open_link(&link).await.status(), 404);
    assert_eq!(s.app.open_link_content(&link).await.status(), 404);
    // Restored, the permissions and the link work again.
    let restored = s
        .app
        .request(Method::POST, &s.path("/restore"), &s.a)
        .send()
        .await
        .expect("restore");
    assert_eq!(restored.status(), 200);
    assert_eq!(s.app.get_as(&s.path(""), &s.b).await.status(), 200);
    assert_eq!(s.app.open_link(&link).await.status(), 200);
}

#[tokio::test]
async fn a_purge_removes_permissions_and_the_link() {
    let Some(s) = shared().await else {
        return;
    };
    let link = enable_link(&s.app, &s.a, &s.doc).await;
    let rows = |table: &'static str| {
        let app = &s.app;
        let doc = s.doc.clone();
        async move {
            sqlx::query_scalar::<_, i64>(&format!(
                "SELECT count(*) FROM {table} WHERE document_id = $1::uuid"
            ))
            .bind(doc)
            .fetch_one(app.pool())
            .await
            .expect("count")
        }
    };
    assert_eq!(rows("document_acl").await, 3);
    assert_eq!(rows("document_share_settings").await, 1);
    s.app.trash(&s.a, &s.path("")).await;
    let purged = s
        .app
        .purge(&s.a, &s.path(""), &crate::support::new_key())
        .await;
    assert_eq!(purged.status(), 204);
    assert_eq!(rows("document_acl").await, 0);
    assert_eq!(rows("document_share_settings").await, 0);
    assert_eq!(s.app.open_link(&link).await.status(), 404);
}

#[tokio::test]
async fn shared_with_me_lists_new_documents_until_seen() {
    let Some(s) = shared().await else {
        return;
    };
    let (second, _) = upload(&s.app, &s.a, "second").await;
    share_ok(&s.app, &s.a, &second, &[B], "viewer").await;
    let page = ok_json(s.app.get_as("/api/v1/shared?limit=1", &s.b).await, 200).await;
    assert_eq!(page["unseen"], 2, "{page}");
    let first = &page["documents"][0];
    assert_eq!(first["id"], second.as_str(), "most recently shared first");
    assert_eq!(first["role"], "viewer");
    assert_eq!(first["new"], true);
    assert_eq!(first["folder_id"], Value::Null);
    assert!(first["shared_at"].as_str().is_some());
    let cursor = page["next_cursor"].as_str().expect("cursor");
    let next = ok_json(
        s.app
            .get_as(&format!("/api/v1/shared?limit=1&cursor={cursor}"), &s.b)
            .await,
        200,
    )
    .await;
    assert_eq!(next["documents"][0]["id"], s.doc.as_str());
    assert_eq!(next["documents"][0]["role"], "editor");
    assert_eq!(next["next_cursor"], Value::Null);

    let seen = s
        .app
        .request(Method::POST, &format!("/api/v1/shared/{second}/seen"), &s.b)
        .send()
        .await
        .expect("seen");
    assert_eq!(seen.status(), 204);
    let page = ok_json(s.app.get_as("/api/v1/shared", &s.b).await, 200).await;
    assert_eq!(page["unseen"], 1);
    assert_eq!(page["documents"][0]["new"], false);
    // The owner's own documents and documents not shared with the caller are not "shared with me".
    let mine = ok_json(s.app.get_as("/api/v1/shared", &s.a).await, 200).await;
    assert_eq!(mine["documents"], json!([]));
    let not_mine = s
        .app
        .request(Method::POST, &format!("/api/v1/shared/{second}/seen"), &s.d)
        .send()
        .await
        .expect("seen");
    assert_eq!(not_mine.status(), 404);
}

#[tokio::test]
async fn a_recipients_copy_lands_in_their_own_root() {
    let Some(s) = shared().await else {
        return;
    };
    let bytes = s
        .app
        .get_as(&s.path("/content"), &s.c)
        .await
        .bytes()
        .await
        .expect("bytes");
    let copy = ok_json(s.app.upload(&s.c, "Bản sao.docx", &bytes, &[]).await, 201).await;
    assert_eq!(copy["role"], "owner");
    assert_eq!(copy["folder_id"], Value::Null);
    assert_eq!(copy["shared"], false);
    let root = ok_json(
        s.app.get_as("/api/v1/folders/root/children", &s.c).await,
        200,
    )
    .await;
    assert_eq!(
        root["documents"].as_array().map(Vec::len),
        Some(1),
        "{root}"
    );
}
