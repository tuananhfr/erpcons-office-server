//! A real server on a random port, a fresh database and an in-process pi-mock per test.
use std::{net::SocketAddr, sync::Arc, time::Duration};

use office_api::{ApiConfig, AppState, router};
use office_db::{PgPool, PoolConfig, testing::TestDb};
use office_identity::{MeEndpointConfig, MeEndpointVerifier, testing::access_token};
use office_storage::FsStorage;
use pi_mock::{Scenario, Server};
use reqwest::{Response, header::AUTHORIZATION};
use serde_json::Value;
use tempfile::TempDir;

pub struct Setup {
    pub api: ApiConfig,
    pub identity: MeEndpointConfig,
}

pub struct TestApp {
    pub pi: Server,
    pub data: TempDir,
    pub http: reqwest::Client,
    base: String,
    pool: PgPool,
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
        let pi = Server::start(Scenario::default());
        let mut setup = Setup {
            api: ApiConfig::default(),
            identity: MeEndpointConfig::new(pi.base_url()),
        };
        tweak(&mut setup);
        let data = tempfile::tempdir().expect("tempdir");
        let storage = FsStorage::open(data.path().join("store"))
            .await
            .expect("storage");
        let identity = MeEndpointVerifier::new(setup.identity).expect("verifier");
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
        let app = router(state).into_make_service_with_connect_info::<SocketAddr>();
        tokio::spawn(async move { axum::serve(listener, app).await.expect("serve") });
        Self {
            pi,
            data,
            http: reqwest::Client::new(),
            base: format!("http://{address}"),
            pool,
            _db: db,
        }
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    pub fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    pub async fn token(&self) -> String {
        access_token(&self.pi).await
    }

    pub async fn get(&self, path: &str) -> Response {
        self.http.get(self.url(path)).send().await.expect("request")
    }

    pub async fn get_as(&self, path: &str, token: &str) -> Response {
        self.http
            .get(self.url(path))
            .header(AUTHORIZATION, format!("Bearer {token}"))
            .send()
            .await
            .expect("request")
    }
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

/// Checks the contract's `Error` body and returns it.
pub async fn expect_error(response: Response, status: u16, code: &str, retryable: bool) -> Value {
    assert_eq!(response.status().as_u16(), status);
    let id = request_id(&response);
    let body: Value = response.json().await.expect("JSON error body");
    assert_eq!(body["code"], code, "{body}");
    assert_eq!(body["retryable"], retryable, "{body}");
    assert_eq!(body["request_id"], id.as_str(), "{body}");
    assert!(body["message_vi"].as_str().is_some_and(|m| !m.is_empty()));
    assert_eq!(body.as_object().map(|o| o.len()), Some(4), "{body}");
    body
}
