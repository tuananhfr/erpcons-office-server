//! Expiries are the owner's: other sharers neither move nor lift them, on the link or on people,
//! and what a time-limited sharer hands out ends no later than its own access.
use serde_json::{Value, json};

use crate::{
    people::{
        delete_link, dialog, grant_of, owner, patch_permission, permission_of, put_link, share_ok,
        sign_in, upload,
    },
    support::{TestApp, expect_error, ok_json},
};

const B: &str = "b.editor@erpcons.test";
const C: &str = "c.viewer@erpcons.test";
const UNTIL: &str = "2999-01-01T00:00:00Z";

/// A's document with B as editor and C as viewer.
async fn setup(app: &TestApp) -> (String, String, String, String) {
    let a = owner(app).await;
    let b = sign_in(app, "usr_b", "Bình", B, true).await;
    sign_in(app, "usr_c", "Chi", C, true).await;
    let (doc, _) = upload(app, &a, "expiries").await;
    share_ok(app, &a, &doc, &[B], "editor").await;
    let body = share_ok(app, &a, &doc, &[C], "viewer").await;
    let c = permission_of(&body, C);
    (a, b, doc, c)
}

#[tokio::test]
async fn only_the_owner_moves_or_ends_a_link_expiry() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let (a, b, doc, _) = setup(&app).await;
    let until = json!({ "expires_at": UNTIL });
    let created = ok_json(put_link(&app, &a, &doc, Some(&until)).await, 200).await;
    let token = created["token"].as_str().expect("token").to_owned();

    // Another sharer may fetch the link as it is ...
    let fetched = ok_json(put_link(&app, &b, &doc, None).await, 200).await;
    assert_eq!(fetched["token"], token.as_str());
    assert_eq!(fetched["expires_at"], UNTIL);
    let same = ok_json(put_link(&app, &b, &doc, Some(&until)).await, 200).await;
    assert_eq!(same["token"], token.as_str());
    // ... but not lift or move its end.
    for body in [
        json!({ "expires_at": null }),
        json!({ "expires_at": "2998-01-01T00:00:00Z" }),
    ] {
        expect_error(
            put_link(&app, &b, &doc, Some(&body)).await,
            403,
            "FORBIDDEN",
            false,
        )
        .await;
    }
    // Nor turn it off, which would let it make a new one without an end.
    assert_eq!(delete_link(&app, &b, &doc).await, 403);
    assert_eq!(app.open_link(&token).await.status(), 200);
    assert_eq!(dialog(&app, &a, &doc).await["link"]["expires_at"], UNTIL);

    // The owner may; then a link without an end is the editor's to manage.
    assert_eq!(delete_link(&app, &a, &doc).await, 204);
    let own = ok_json(put_link(&app, &b, &doc, None).await, 200).await;
    assert_eq!(own["expires_at"], Value::Null);
    assert_eq!(delete_link(&app, &b, &doc).await, 204);
}

#[tokio::test]
async fn a_time_limited_sharer_hands_out_nothing_longer_than_its_own_access() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let (a, b, doc, c) = setup(&app).await;
    let b_id = permission_of(&dialog(&app, &a, &doc).await, B);
    assert_eq!(
        patch_permission(&app, &a, &doc, &b_id, &json!({ "expires_at": UNTIL })).await,
        200
    );

    // New people, with or without an account, get B's end.
    sign_in(&app, "usr_d", "Dũng", "d@erpcons.test", true).await;
    share_ok(
        &app,
        &b,
        &doc,
        &["d@erpcons.test", "new@erpcons.test"],
        "viewer",
    )
    .await;
    for email in ["d@erpcons.test", "new@erpcons.test"] {
        assert_eq!(
            grant_of(&app, &a, &doc, email).await["expires_at"],
            UNTIL,
            "{email}"
        );
    }
    // Raising someone's role is handing out more: it ends with B too. Lowering one changes no end.
    assert_eq!(
        patch_permission(&app, &b, &doc, &c, &json!({ "role": "editor" })).await,
        200
    );
    assert_eq!(grant_of(&app, &a, &doc, C).await["expires_at"], UNTIL);
    let e = permission_of(&dialog(&app, &a, &doc).await, "d@erpcons.test");
    assert_eq!(
        patch_permission(
            &app,
            &a,
            &doc,
            &e,
            &json!({ "role": "editor", "expires_at": null })
        )
        .await,
        200
    );
    assert_eq!(
        patch_permission(&app, &b, &doc, &e, &json!({ "role": "viewer" })).await,
        200
    );
    assert_eq!(
        grant_of(&app, &a, &doc, "d@erpcons.test").await["expires_at"],
        Value::Null
    );

    // A link B makes ends with B, whatever B asks for.
    let link = ok_json(put_link(&app, &b, &doc, None).await, 200).await;
    assert_eq!(link["expires_at"], UNTIL);
    assert_eq!(delete_link(&app, &a, &doc).await, 204);
    let later = json!({ "expires_at": "3500-01-01T00:00:00Z" });
    let link = ok_json(put_link(&app, &b, &doc, Some(&later)).await, 200).await;
    assert_eq!(link["expires_at"], UNTIL);
    let sooner = json!({ "expires_at": "2990-01-01T00:00:00Z" });
    assert_eq!(delete_link(&app, &a, &doc).await, 204);
    let link = ok_json(put_link(&app, &b, &doc, Some(&sooner)).await, 200).await;
    assert_eq!(link["expires_at"], "2990-01-01T00:00:00Z");
}
