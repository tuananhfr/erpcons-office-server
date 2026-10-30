//! The people of the sharing tests: A owns, the others get roles on A's document.
use office_identity::testing::DEFAULT_SUBJECT;
use reqwest::{Method, Response};
use serde_json::{Value, json};

use crate::support::{TestApp, docx, etag, ok_json};

pub const OWNER_EMAIL: &str = "an.owner@erpcons.test";

/// Signs in a new PI user whose email PI reports as `verified`; returns its token.
pub async fn sign_in(app: &TestApp, sub: &str, name: &str, email: &str, verified: bool) -> String {
    app.pi.add_user(sub, name);
    app.pi.set_email(sub, email, verified);
    let token = app.pi.token_for(sub);
    assert_eq!(app.get_as("/api/v1/me", &token).await.status(), 200);
    token
}

/// The default user, A, signed in with a verified email.
pub async fn owner(app: &TestApp) -> String {
    app.pi.set_email(DEFAULT_SUBJECT, OWNER_EMAIL, true);
    let token = app.token().await;
    assert_eq!(app.get_as("/api/v1/me", &token).await.status(), 200);
    token
}

/// Uploads a document; returns its id and ETag.
pub async fn upload(app: &TestApp, token: &str, marker: &str) -> (String, String) {
    let response = app
        .upload(token, &format!("{marker}.docx"), &docx(marker), &[])
        .await;
    let tag = etag(&response);
    let doc = ok_json(response, 201).await;
    (doc["id"].as_str().expect("id").to_owned(), tag)
}

pub async fn share(
    app: &TestApp,
    token: &str,
    doc: &str,
    emails: &[&str],
    role: &str,
    notify: bool,
) -> Response {
    app.send_json(
        Method::POST,
        &format!("/api/v1/documents/{doc}/permissions"),
        token,
        &json!({ "emails": emails, "role": role, "notify": notify }),
    )
    .await
}

/// Shares and asserts it worked; returns the `Permissions` body.
pub async fn share_ok(app: &TestApp, token: &str, doc: &str, emails: &[&str], role: &str) -> Value {
    ok_json(share(app, token, doc, emails, role, false).await, 200).await
}

/// The id of the permission held by `email` in a `Permissions` body.
pub fn permission_of(permissions: &Value, email: &str) -> String {
    permissions["permissions"]
        .as_array()
        .expect("permissions")
        .iter()
        .find(|p| p["email"] == email)
        .and_then(|p| p["id"].as_str())
        .expect("permission of email")
        .to_owned()
}

pub async fn status(response: Response) -> u16 {
    response.status().as_u16()
}

/// Turns the public link on; returns the token.
pub async fn enable_link(app: &TestApp, token: &str, doc: &str) -> String {
    let response = app
        .request(Method::PUT, &format!("/api/v1/documents/{doc}/link"), token)
        .send()
        .await
        .expect("enable link");
    let body = ok_json(response, 200).await;
    assert_eq!(body["role"], "viewer");
    body["token"].as_str().expect("token").to_owned()
}

/// The share dialog as `token` sees it.
pub async fn dialog(app: &TestApp, token: &str, doc: &str) -> Value {
    ok_json(
        app.get_as(&format!("/api/v1/documents/{doc}/permissions"), token)
            .await,
        200,
    )
    .await
}

/// The permission of `email` in the owner's dialog, or `null`.
pub async fn grant_of(app: &TestApp, owner: &str, doc: &str, email: &str) -> Value {
    dialog(app, owner, doc).await["permissions"]
        .as_array()
        .expect("permissions")
        .iter()
        .find(|p| p["email"] == email)
        .cloned()
        .unwrap_or(Value::Null)
}

/// `PATCH` of one permission; returns the status.
pub async fn patch_permission(
    app: &TestApp,
    token: &str,
    doc: &str,
    permission: &str,
    body: &Value,
) -> u16 {
    status(
        app.send_json(
            Method::PATCH,
            &format!("/api/v1/documents/{doc}/permissions/{permission}"),
            token,
            body,
        )
        .await,
    )
    .await
}

/// `PUT /link`, with a body only when given.
pub async fn put_link(app: &TestApp, token: &str, doc: &str, body: Option<&Value>) -> Response {
    let mut request = app.request(Method::PUT, &format!("/api/v1/documents/{doc}/link"), token);
    if let Some(body) = body {
        request = request.json(body);
    }
    request.send().await.expect("put link")
}

pub async fn delete_link(app: &TestApp, token: &str, doc: &str) -> u16 {
    status(
        app.request(
            Method::DELETE,
            &format!("/api/v1/documents/{doc}/link"),
            token,
        )
        .send()
        .await
        .expect("delete link"),
    )
    .await
}
