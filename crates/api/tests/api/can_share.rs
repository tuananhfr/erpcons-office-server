//! `Document.can_share`: whether the site offers the Share button, by the same rule as the share
//! dialog's `can_share`.
use reqwest::Method;
use serde_json::{Value, json};

use crate::{
    people::{dialog, enable_link, owner, share_ok, sign_in},
    support::{TestApp, docx, ok_json},
};

async fn document(app: &TestApp, token: &str, doc: &str) -> Value {
    ok_json(
        app.get_as(&format!("/api/v1/documents/{doc}"), token).await,
        200,
    )
    .await
}

/// `can_share` of the document and of the dialog, which must agree.
async fn can_share(app: &TestApp, token: &str, doc: &str) -> bool {
    let from_document = document(app, token, doc).await["can_share"].clone();
    let from_dialog = dialog(app, token, doc).await["can_share"].clone();
    assert_eq!(from_document, from_dialog);
    from_document.as_bool().expect("can_share")
}

async fn editors_can_share(app: &TestApp, owner: &str, doc: &str, on: bool) {
    let response = app
        .send_json(
            Method::PUT,
            &format!("/api/v1/documents/{doc}/share-settings"),
            owner,
            &json!({ "editors_can_share": on, "viewers_can_download": true }),
        )
        .await;
    assert_eq!(response.status(), 200);
}

#[tokio::test]
async fn the_document_says_who_may_share_as_the_dialog_does() {
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let a = owner(&app).await;
    let b = sign_in(&app, "usr_b", "Bình", "b@erpcons.test", true).await;
    let c = sign_in(&app, "usr_c", "Chi", "c@erpcons.test", true).await;
    let d = sign_in(&app, "usr_d", "Dũng", "d@erpcons.test", true).await;
    let created = app.upload(&a, "share.docx", &docx("share"), &[]).await;
    let created = ok_json(created, 201).await;
    assert_eq!(created["can_share"], true);
    let doc = created["id"].as_str().expect("id").to_owned();
    share_ok(&app, &a, &doc, &["b@erpcons.test"], "editor").await;
    share_ok(&app, &a, &doc, &["c@erpcons.test"], "commenter").await;
    share_ok(&app, &a, &doc, &["d@erpcons.test"], "viewer").await;

    for (token, want) in [(&a, true), (&b, true), (&c, false), (&d, false)] {
        assert_eq!(can_share(&app, token, &doc).await, want);
    }
    let shared = ok_json(app.get_as("/api/v1/shared", &b).await, 200).await;
    assert_eq!(shared["documents"][0]["can_share"], true);

    // The owner stops editors from sharing: the editor's next read says so everywhere.
    editors_can_share(&app, &a, &doc, false).await;
    assert!(!can_share(&app, &b, &doc).await);
    assert!(can_share(&app, &a, &doc).await);
    let shared = ok_json(app.get_as("/api/v1/shared", &b).await, 200).await;
    assert_eq!(shared["documents"][0]["can_share"], false);
    let renamed = app
        .send_json(
            Method::PATCH,
            &format!("/api/v1/documents/{doc}"),
            &b,
            &json!({ "title": "renamed" }),
        )
        .await;
    assert_eq!(ok_json(renamed, 200).await["can_share"], false);
    editors_can_share(&app, &a, &doc, true).await;
    assert!(can_share(&app, &b, &doc).await);

    // Anyone holding the link only views.
    let link = enable_link(&app, &a, &doc).await;
    let viewed = ok_json(app.open_link(&link).await, 200).await;
    assert_eq!(viewed["role"], "link");
    assert_eq!(viewed["can_share"], false);

    let root = ok_json(app.get_as("/api/v1/folders/root/children", &a).await, 200).await;
    assert_eq!(root["documents"][0]["can_share"], true);
    // A document in the trash is shared with no one.
    app.trash(&a, &format!("/api/v1/documents/{doc}")).await;
    assert!(!can_share(&app, &a, &doc).await);
}
