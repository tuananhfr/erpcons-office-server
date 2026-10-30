//! A real server on a random port, a fresh database and an in-process PI stand-in per test.
use std::{net::SocketAddr, path::PathBuf, sync::Arc, time::Duration};

use office_api::{ApiConfig, AppState, RateConfig, router};
use office_db::{PgPool, PoolConfig, testing::TestDb};
use office_documents::{
    Documents,
    intake::testing::{ZipEntry, zip},
};
use office_identity::{JwtVerifier, JwtVerifierConfig, testing::FakePi};
use office_storage::FsStorage;
use reqwest::{Method, RequestBuilder, Response, header::AUTHORIZATION};
use serde_json::Value;
use tempfile::TempDir;
use uuid::Uuid;

/// Far above what any test sends, so only the rate-limit tests meet a limit.
const UNLIMITED: RateConfig = RateConfig {
    burst: 100_000,
    per_second: 100_000.0,
};

pub struct Setup {
    pub api: ApiConfig,
    pub identity: JwtVerifierConfig,
}

pub struct TestApp {
    pub pi: FakePi,
    pub data: TempDir,
    pub http: reqwest::Client,
    base: String,
    pool: PgPool,
    state: AppState,
    // Dropped last: drops the database.
    _db: Option<TestDb>,
}

impl TestApp {
    /// App on a fresh database; `None` when `TEST_DATABASE_URL` is unset.
    pub async fn spawn() -> Option<Self> {
        Self::spawn_with(|_| {}).await
    }

    pub async fn spawn_with(tweak: impl FnOnce(&mut Setup)) -> Option<Self> {
        let db = TestDb::create().await?;
        let pool = db.pool().clone();
        Some(Self::start(pool, Some(db), tweak).await)
    }

    /// App whose database can never be reached.
    pub async fn spawn_without_database() -> Self {
        let mut config = PoolConfig::new("postgres://office:unused@127.0.0.1:1/office");
        config.acquire_timeout = Duration::from_millis(500);
        let pool = office_db::connect_lazy(&config).expect("lazy pool");
        Self::start(pool, None, |_| {}).await
    }

    async fn start(pool: PgPool, db: Option<TestDb>, tweak: impl FnOnce(&mut Setup)) -> Self {
        let pi = FakePi::start().await;
        let mut setup = Setup {
            api: ApiConfig {
                ip_rate: UNLIMITED,
                actor_rate: UNLIMITED,
                verify_rate: UNLIMITED,
                max_concurrent_uploads: 1000,
                link_key: Some(office_documents::LinkKey::from_bytes([42; 32])),
                ..ApiConfig::default()
            },
            identity: JwtVerifierConfig::new(pi.issuer()),
        };
        tweak(&mut setup);
        let data = tempfile::tempdir().expect("tempdir");
        let storage = FsStorage::open(data.path().join("store"))
            .await
            .expect("storage");
        let identity = JwtVerifier::new(setup.identity).expect("verifier");
        let state = AppState::new(
            pool.clone(),
            Arc::new(storage),
            Arc::new(identity),
            setup.api,
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let address = listener.local_addr().expect("address");
        let app = router(state.clone()).into_make_service_with_connect_info::<SocketAddr>();
        tokio::spawn(async move { axum::serve(listener, app).await.expect("serve") });
        Self {
            pi,
            data,
            http: reqwest::Client::new(),
            base: format!("http://{address}"),
            pool,
            state,
            _db: db,
        }
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// The document service behind the routes, for running jobs and arming fail points.
    pub fn documents(&self) -> &Arc<Documents> {
        self.state.documents()
    }

    pub fn store_dir(&self) -> PathBuf {
        self.data.path().join("store")
    }

    pub fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    pub async fn token(&self) -> String {
        self.pi.token()
    }

    /// Token of another PI user, added to the stand-in.
    pub async fn token_of(&self, user_id: &str, display_name: &str) -> String {
        self.pi.add_user(user_id, display_name);
        self.pi.token_for(user_id)
    }

    pub async fn get(&self, path: &str) -> Response {
        self.http.get(self.url(path)).send().await.expect("request")
    }

    /// `GET /link` with a public link token, as the site sends it.
    pub async fn open_link(&self, link: &str) -> Response {
        self.link_get("/api/v1/link", link).await
    }

    pub async fn open_link_content(&self, link: &str) -> Response {
        self.link_get("/api/v1/link/content", link).await
    }

    async fn link_get(&self, path: &str, link: &str) -> Response {
        self.http
            .get(self.url(path))
            .header("x-link-token", link)
            .send()
            .await
            .expect("request")
    }

    pub async fn get_as(&self, path: &str, token: &str) -> Response {
        self.request(Method::GET, path, token)
            .send()
            .await
            .expect("request")
    }

    pub fn request(&self, method: Method, path: &str, token: &str) -> RequestBuilder {
        self.http
            .request(method, self.url(path))
            .header(AUTHORIZATION, format!("Bearer {token}"))
    }

    pub async fn send_json(
        &self,
        method: Method,
        path: &str,
        token: &str,
        body: &Value,
    ) -> Response {
        self.request(method, path, token)
            .json(body)
            .send()
            .await
            .expect("request")
    }

    /// POST with a fresh `Idempotency-Key`.
    pub async fn post_json(&self, path: &str, token: &str, body: &Value) -> Response {
        self.post_json_keyed(path, token, &new_key(), body).await
    }

    pub async fn post_json_keyed(
        &self,
        path: &str,
        token: &str,
        key: &str,
        body: &Value,
    ) -> Response {
        self.request(Method::POST, path, token)
            .header("idempotency-key", key)
            .json(body)
            .send()
            .await
            .expect("request")
    }

    pub async fn create_folder(&self, token: &str, parent: Option<&str>, name: &str) -> Value {
        let response = self
            .post_json(
                "/api/v1/folders",
                token,
                &serde_json::json!({ "parent_id": parent, "name": name }),
            )
            .await;
        ok_json(response, 201).await
    }

    pub async fn create_blank(
        &self,
        token: &str,
        kind: &str,
        title: Option<&str>,
        folder: Option<&str>,
    ) -> Value {
        let body = serde_json::json!({ "kind": kind, "title": title, "folder_id": folder });
        ok_json(self.post_json("/api/v1/documents", token, &body).await, 201).await
    }

    /// Multipart upload of `bytes` named `file_name`, with optional extra text fields.
    pub async fn upload(
        &self,
        token: &str,
        file_name: &str,
        bytes: &[u8],
        fields: &[(&str, &str)],
    ) -> Response {
        self.upload_keyed(token, &new_key(), file_name, bytes, fields)
            .await
    }

    pub async fn upload_keyed(
        &self,
        token: &str,
        key: &str,
        file_name: &str,
        bytes: &[u8],
        fields: &[(&str, &str)],
    ) -> Response {
        let (content_type, body) = multipart(fields, Some((file_name, bytes)));
        self.request(Method::POST, "/api/v1/documents", token)
            .header("idempotency-key", key)
            .header("content-type", content_type)
            .body(body)
            .send()
            .await
            .expect("request")
    }

    pub async fn put_content(
        &self,
        token: &str,
        id: &str,
        etag: Option<&str>,
        bytes: Vec<u8>,
    ) -> Response {
        let mut request = self.request(
            Method::PUT,
            &format!("/api/v1/documents/{id}/content"),
            token,
        );
        if let Some(etag) = etag {
            request = request.header("if-match", etag);
        }
        request.body(bytes).send().await.expect("request")
    }

    /// `DELETE` (trash) of a folder or document path; asserts 204.
    pub async fn trash(&self, token: &str, path: &str) {
        let response = self
            .request(Method::DELETE, path, token)
            .send()
            .await
            .expect("trash");
        assert_eq!(response.status(), 204, "trash {path}");
    }

    /// `POST {path}/purge` with `key`.
    pub async fn purge(&self, token: &str, path: &str, key: &str) -> Response {
        self.request(Method::POST, &format!("{path}/purge"), token)
            .header("idempotency-key", key)
            .send()
            .await
            .expect("purge")
    }

    pub async fn count(&self, sql: &str) -> i64 {
        sqlx::query_scalar(sql)
            .fetch_one(self.pool())
            .await
            .expect("count query")
    }
}

pub fn new_key() -> String {
    Uuid::new_v4().to_string()
}

/// A `multipart/form-data` body: text fields first, then the file part.
pub fn multipart(fields: &[(&str, &str)], file: Option<(&str, &[u8])>) -> (String, Vec<u8>) {
    let boundary = "office-test-boundary-5d1c";
    let mut body = Vec::new();
    for (name, value) in fields {
        body.extend_from_slice(
            format!(
                "--{boundary}\r\ncontent-disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n"
            )
            .as_bytes(),
        );
    }
    if let Some((file_name, bytes)) = file {
        body.extend_from_slice(
            format!(
                "--{boundary}\r\ncontent-disposition: form-data; name=\"file\"; filename=\"{file_name}\"\r\ncontent-type: application/octet-stream\r\n\r\n"
            )
            .as_bytes(),
        );
        body.extend_from_slice(bytes);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    (format!("multipart/form-data; boundary={boundary}"), body)
}

/// A minimal OOXML-shaped ZIP; `marker` makes its bytes (and sha256) distinct.
pub fn docx(marker: &str) -> Vec<u8> {
    zip(&[
        ZipEntry::stored("[Content_Types].xml", b"<Types/>"),
        ZipEntry::stored("word/document.xml", marker.as_bytes()),
    ])
}

pub fn pdf(marker: &str) -> Vec<u8> {
    format!("%PDF-1.4\n% {marker}\n%%EOF\n").into_bytes()
}

/// Files under the object store, temp area excluded.
pub fn stored_objects(root: &std::path::Path) -> usize {
    fn walk(dir: &std::path::Path, out: &mut usize) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if entry.file_name() != "tmp" {
                    walk(&path, out);
                }
            } else {
                *out += 1;
            }
        }
    }
    let mut n = 0;
    walk(root, &mut n);
    n
}

pub fn request_id(response: &Response) -> String {
    response
        .headers()
        .get("x-request-id")
        .expect("X-Request-Id")
        .to_str()
        .expect("ascii")
        .to_owned()
}

pub fn etag(response: &Response) -> String {
    response
        .headers()
        .get("etag")
        .expect("ETag")
        .to_str()
        .expect("ascii")
        .to_owned()
}

/// Asserts the status and returns the JSON body.
pub async fn ok_json(response: Response, status: u16) -> Value {
    let actual = response.status().as_u16();
    let text = response.text().await.expect("body");
    assert_eq!(actual, status, "{text}");
    serde_json::from_str(&text).expect("JSON body")
}

/// Checks the contract's `Error` body and returns it.
pub async fn expect_error(response: Response, status: u16, code: &str, retryable: bool) -> Value {
    let body = error_body(response, status, code, retryable).await;
    assert_eq!(body.as_object().map(|o| o.len()), Some(4), "{body}");
    body
}

/// A `VERSION_CONFLICT` body; returns its `latest_version`.
pub async fn expect_conflict(response: Response) -> i64 {
    let body = error_body(response, 409, "VERSION_CONFLICT", false).await;
    assert_eq!(body.as_object().map(|o| o.len()), Some(5), "{body}");
    body["latest_version"].as_i64().expect("latest_version")
}

async fn error_body(response: Response, status: u16, code: &str, retryable: bool) -> Value {
    assert_eq!(response.status().as_u16(), status);
    let id = request_id(&response);
    let body: Value = response.json().await.expect("JSON error body");
    assert_eq!(body["code"], code, "{body}");
    assert_eq!(body["retryable"], retryable, "{body}");
    assert_eq!(body["request_id"], id.as_str(), "{body}");
    assert!(body["message_vi"].as_str().is_some_and(|m| !m.is_empty()));
    body
}
