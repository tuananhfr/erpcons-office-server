//! Logs and audit rows never contain a token, a display name, a folder name, a document title or
//! file bytes. A test binary of its own: its subscriber is process-wide, which a thread-local one
//! cannot reliably be while other tests run.
#[path = "api/support.rs"]
#[allow(
    dead_code,
    reason = "shared with the api test binary; this one uses only part of it"
)]
mod support;

use std::{
    io,
    sync::{Arc, Mutex, PoisonError},
    time::Duration,
};

use office_identity::testing::DEFAULT_NAME;
use reqwest::Method;
use serde_json::{Value, json};

use crate::support::{TestApp, docx, etag, request_id};

#[derive(Clone, Default)]
struct LogCapture(Arc<Mutex<Vec<u8>>>);

impl LogCapture {
    /// Captures every event of every crate, at TRACE, for the rest of the process.
    fn install_global(&self) {
        let writer = self.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .finish();
        tracing::subscriber::set_global_default(subscriber).expect("only subscriber");
    }

    fn text(&self) -> String {
        let bytes = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        String::from_utf8_lossy(&bytes).into_owned()
    }
}

impl io::Write for LogCapture {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn tokens_and_names_stay_out_of_logs_and_audit() {
    let logs = LogCapture::default();
    logs.install_global();
    let Some(app) = TestApp::spawn_with(|s| {
        s.identity.jwks_min_refresh = Duration::ZERO;
        s.api.signup = office_api::SignupPolicy {
            enabled: true,
            daily_cap: 10,
        };
    })
    .await
    else {
        return;
    };
    let token = app.token().await;
    let rejected = "canary-token-never-issued-7f3a9c";

    let ok = app.get_as("/api/v1/me", &token).await;
    assert_eq!(ok.status(), 200);
    let ok_id = request_id(&ok);
    let (canaries, doc_id) = touch_documents(&app, &token).await;
    let shared = share_and_link(&app, &token, &doc_id).await;
    let signed_up = sign_up(&app).await;
    assert_eq!(app.get_as("/api/v1/me", rejected).await.status(), 401);
    // A key PI rotated in while it is down cannot be fetched: the check is unavailable.
    app.pi.rotate("2027-04", true);
    app.pi.set_down(true);
    let fresh = app.token().await;
    assert_eq!(app.get_as("/api/v1/me", &fresh).await.status(), 503);

    let mut secrets = vec![token.as_str(), fresh.as_str(), rejected, DEFAULT_NAME];
    secrets.extend(canaries);
    secrets.extend(shared.iter().map(String::as_str));
    secrets.extend(signed_up.iter().map(String::as_str));
    let text = logs.text();
    assert!(text.contains(&ok_id), "request spans are logged: {text}");
    assert!(
        text.contains("sqlx::query"),
        "statements are logged at TRACE: {text}"
    );
    for secret in secrets.iter().copied() {
        assert!(!text.contains(secret), "{secret:?} leaked into the logs");
    }

    let rows: Vec<String> = sqlx::query_scalar("SELECT row_to_json(a)::text FROM audit_events a")
        .fetch_all(app.pool())
        .await
        .expect("audit rows");
    assert_eq!(rows.len(), 14, "{rows:?}");
    for row in &rows {
        for secret in secrets.iter().copied() {
            assert!(!row.contains(secret), "{secret:?} leaked into audit: {row}");
        }
    }
    let outcomes: Vec<(String, String)> =
        sqlx::query_as("SELECT action, result FROM audit_events ORDER BY action, result")
            .fetch_all(app.pool())
            .await
            .expect("outcomes");
    assert_eq!(
        outcomes,
        [
            ("auth.verify".into(), "denied".into()),
            ("auth.verify".into(), "error".into()),
            ("document.create".into(), "success".into()),
            ("document.link_disable".into(), "success".into()),
            ("document.link_enable".into(), "success".into()),
            ("document.save".into(), "success".into()),
            ("document.share".into(), "success".into()),
            ("document.update".into(), "success".into()),
            ("folder.create".into(), "success".into()),
            ("folder.update".into(), "success".into()),
            ("link.content".into(), "success".into()),
            ("link.open".into(), "success".into()),
            ("me.read".into(), "success".into()),
            ("me.read".into(), "success".into()),
        ]
    );
}

/// Names, titles and file contents that must appear in no log line or audit row.
const CANARIES: [&str; 5] = [
    "Canary thư mục 3f9a",
    "Canary đổi tên 81c2",
    "Canary tiêu đề 5d07",
    "Canary tệp 2b6e.docx",
    "canary-bytes-e41f",
];

/// Creates, renames and saves through every mutating route that carries a name or bytes; returns
/// the canaries and the document's id.
async fn touch_documents(app: &TestApp, token: &str) -> ([&'static str; 5], String) {
    let [folder_name, renamed, title, file_name, marker] = CANARIES;
    let folder = app
        .post_json("/api/v1/folders", token, &json!({ "name": folder_name }))
        .await;
    assert_eq!(folder.status(), 201);
    let folder: Value = folder.json().await.expect("folder");
    let folder_id = folder["id"].as_str().expect("id");
    let patch = app
        .send_json(
            Method::PATCH,
            &format!("/api/v1/folders/{folder_id}"),
            token,
            &json!({ "name": renamed }),
        )
        .await;
    assert_eq!(patch.status(), 200);
    let doc = app
        .upload(token, file_name, &docx(marker), &[("folder_id", folder_id)])
        .await;
    assert_eq!(doc.status(), 201);
    let tag = etag(&doc);
    let doc: Value = doc.json().await.expect("document");
    let doc_id = doc["id"].as_str().expect("id");
    let patch = app
        .send_json(
            Method::PATCH,
            &format!("/api/v1/documents/{doc_id}"),
            token,
            &json!({ "title": title }),
        )
        .await;
    assert_eq!(patch.status(), 200);
    let saved = app
        .put_content(token, doc_id, Some(&tag), docx(&format!("{marker} v2")))
        .await;
    assert_eq!(saved.status(), 200);
    (CANARIES, doc_id.to_owned())
}

/// Shares by email (with an account and pending) and opens the public link; returns what must
/// stay out: the emails, the recipient's name and token, and the link token.
async fn share_and_link(app: &TestApp, token: &str, doc_id: &str) -> Vec<String> {
    let email = "canary.email.9b1d@erpcons.test";
    let pending = "canary.pending.4e7a@erpcons.test";
    let name = "Canary người nhận c0de";
    app.pi.add_user("usr_canary", name);
    app.pi.set_email("usr_canary", email, true);
    let recipient = app.pi.token_for("usr_canary");
    assert_eq!(app.get_as("/api/v1/me", &recipient).await.status(), 200);
    let shared = app
        .send_json(
            Method::POST,
            &format!("/api/v1/documents/{doc_id}/permissions"),
            token,
            &json!({ "emails": [email, pending], "role": "viewer", "notify": true, "message": "Canary lời nhắn 6a1f" }),
        )
        .await;
    assert_eq!(shared.status(), 200);
    let link = app
        .request(
            Method::PUT,
            &format!("/api/v1/documents/{doc_id}/link"),
            token,
        )
        .send()
        .await
        .expect("link");
    let link: Value = link.json().await.expect("link body");
    let link = link["token"].as_str().expect("token").to_owned();
    assert_eq!(app.open_link(&link).await.status(), 200);
    assert_eq!(app.open_link_content(&link).await.status(), 200);
    let off = app
        .request(
            Method::DELETE,
            &format!("/api/v1/documents/{doc_id}/link"),
            token,
        )
        .send()
        .await
        .expect("disable");
    assert_eq!(off.status(), 204);
    vec![
        email.to_owned(),
        pending.to_owned(),
        name.to_owned(),
        recipient,
        link,
        "Canary lời nhắn 6a1f".to_owned(),
    ]
}

/// Signs up and verifies; returns the name, address, password and verification token.
async fn sign_up(app: &TestApp) -> Vec<String> {
    use office_mail::SignupTokens;

    let (name, email, password) = (
        "Canary đăng ký 71ac",
        "canary.signup.5f2e@erpcons.test",
        "canary-mật-khẩu-0d93",
    );
    let response = app
        .post_public(
            "/api/v1/signup",
            &json!({ "name": name, "email": email, "password": password }),
        )
        .await;
    assert_eq!(response.status(), 202);
    let id = sqlx::query_scalar("SELECT id FROM signups")
        .fetch_one(app.pool())
        .await
        .expect("sign-up");
    let token = app
        .signups()
        .issue(id)
        .await
        .expect("issue")
        .expect("pending")
        .token;
    let verified = app
        .post_public("/api/v1/signup/verify", &json!({ "token": token }))
        .await;
    assert_eq!(verified.status(), 200);
    vec![name.into(), email.into(), password.into(), token]
}
