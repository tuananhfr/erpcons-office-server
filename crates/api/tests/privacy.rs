//! Logs and audit rows never contain a token or a display name. A test binary of its own: its
//! subscriber is process-wide, which a thread-local one cannot reliably be while other tests run.
#[path = "api/support.rs"]
#[allow(
    dead_code,
    reason = "shared with the api test binary; this one uses only part of it"
)]
mod support;

use std::{
    io,
    sync::{Arc, Mutex, PoisonError},
};

use pi_mock::Fault;

use crate::support::{TestApp, request_id};

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
    let Some(app) = TestApp::spawn().await else {
        return;
    };
    let token = app.token().await;
    let rejected = "canary-token-never-issued-7f3a9c";

    let ok = app.get_as("/api/v1/me", &token).await;
    assert_eq!(ok.status(), 200);
    let ok_id = request_id(&ok);
    assert_eq!(app.get_as("/api/v1/me", rejected).await.status(), 401);
    app.pi
        .add_fault(Fault::problem("GET", "/v1/me", 503, "unavailable"));
    let fresh = app.token().await;
    assert_eq!(app.get_as("/api/v1/me", &fresh).await.status(), 503);

    let secrets = [token.as_str(), fresh.as_str(), rejected, "Nguyễn Văn An"];
    let text = logs.text();
    assert!(text.contains(&ok_id), "request spans are logged: {text}");
    assert!(
        text.contains("sqlx::query"),
        "statements are logged at TRACE: {text}"
    );
    for secret in secrets {
        assert!(!text.contains(secret), "{secret:?} leaked into the logs");
    }

    let rows: Vec<String> = sqlx::query_scalar("SELECT row_to_json(a)::text FROM audit_events a")
        .fetch_all(app.pool())
        .await
        .expect("audit rows");
    assert_eq!(rows.len(), 3, "{rows:?}");
    for row in &rows {
        for secret in secrets {
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
            ("me.read".into(), "success".into()),
        ]
    );
}
