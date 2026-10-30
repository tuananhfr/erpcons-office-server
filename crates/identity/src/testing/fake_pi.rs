use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex, MutexGuard, PoisonError,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    task::JoinHandle,
};

use super::SigningKey;

/// The signed-in user of a fresh `FakePi`; PI subjects are account UUIDs.
pub const DEFAULT_SUBJECT: &str = "4f1c2a8e-6b7d-4e0a-9c3f-2d5b8a1e7f60";
pub const DEFAULT_NAME: &str = "Nguyễn Văn An";
/// Office's web client, the `azp` of the tokens `FakePi` issues by default.
pub const OFFICE_CLIENT: &str = "erpcons-office-web";

const MAX_HEAD_BYTES: usize = 16 * 1024;

/// A PI stand-in on a random local port, following PI's code: access tokens with the claims of
/// `AuthorizationServer::tokens`, JWKS as `Jwt::jwks`, userinfo as `OidcController::userinfo`.
/// Stops when dropped.
pub struct FakePi {
    base_url: String,
    shared: Arc<Mutex<Shared>>,
    jti: AtomicU64,
    task: JoinHandle<()>,
}

struct Shared {
    /// The first key signs; every published key is in the JWKS.
    keys: Vec<(SigningKey, bool)>,
    users: HashMap<String, FakeUser>,
    down: bool,
    redirect_to: Option<String>,
    delay: Duration,
    requests: Vec<String>,
}

struct FakeUser {
    name: String,
    locked: bool,
    /// Address and whether PI marks it verified; served with the `email` scope.
    email: Option<(String, bool)>,
}

impl FakePi {
    pub async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind fake PI");
        let base_url = format!("http://{}", listener.local_addr().expect("fake PI address"));
        let mut users = HashMap::new();
        users.insert(
            DEFAULT_SUBJECT.to_owned(),
            FakeUser {
                name: DEFAULT_NAME.to_owned(),
                locked: false,
                email: None,
            },
        );
        let shared = Arc::new(Mutex::new(Shared {
            keys: vec![(SigningKey::generate("2026-10"), true)],
            users,
            down: false,
            redirect_to: None,
            delay: Duration::ZERO,
            requests: Vec::new(),
        }));
        let serving = Arc::clone(&shared);
        let task = tokio::spawn(async move {
            while let Ok((socket, _)) = listener.accept().await {
                tokio::spawn(answer(socket, Arc::clone(&serving)));
            }
        });
        Self {
            base_url,
            shared,
            jti: AtomicU64::new(0),
            task,
        }
    }

    /// PI's issuer: its base URL, as `pi_id_issuer` would be.
    pub fn issuer(&self) -> &str {
        &self.base_url
    }

    pub fn add_user(&self, sub: &str, name: &str) {
        self.lock().users.insert(
            sub.to_owned(),
            FakeUser {
                name: name.to_owned(),
                locked: false,
                email: None,
            },
        );
    }

    /// Gives the account an email, which userinfo reports with `email_verified: verified`.
    pub fn set_email(&self, sub: &str, email: &str, verified: bool) {
        if let Some(user) = self.lock().users.get_mut(sub) {
            user.email = Some((email.to_owned(), verified));
        }
    }

    /// Userinfo then says nothing about an email, as without the `email` scope.
    pub fn clear_email(&self, sub: &str) {
        if let Some(user) = self.lock().users.get_mut(sub) {
            user.email = None;
        }
    }

    /// Locks the account: PI's userinfo then refuses its tokens; their signature stays valid.
    pub fn lock_user(&self, sub: &str) {
        if let Some(user) = self.lock().users.get_mut(sub) {
            user.locked = true;
        }
    }

    /// An access token of the default user for Office's web client.
    pub fn token(&self) -> String {
        self.token_for(DEFAULT_SUBJECT)
    }

    pub fn token_for(&self, sub: &str) -> String {
        self.sign(&self.claims_for(sub))
    }

    /// The claims PI puts in an access token of a client without a product.
    pub fn claims_for(&self, sub: &str) -> Value {
        let now = unix_now();
        json!({
            "iss": self.base_url,
            "sub": sub,
            "aud": ["pi-platform"],
            "azp": OFFICE_CLIENT,
            "scope": "openid profile email offline_access",
            "org_context": null,
            "iat": now,
            "nbf": now,
            "exp": now + 600,
            "jti": format!("jti-{}", self.jti.fetch_add(1, Ordering::Relaxed)),
        })
    }

    /// `claims` signed with the current key under PI's header.
    pub fn sign(&self, claims: &Value) -> String {
        let shared = self.lock();
        let key = &shared.keys[0].0;
        key.sign(&key.header(), claims)
    }

    /// `claims` signed with the current key under any `header`.
    pub fn sign_with_header(&self, header: &Value, claims: &Value) -> String {
        self.lock().keys[0].0.sign(header, claims)
    }

    /// The `kid` of the current signing key.
    pub fn kid(&self) -> String {
        self.lock().keys[0].0.kid().to_owned()
    }

    /// A new signing key under `kid`, published in the JWKS or not; older keys stay published.
    pub fn rotate(&self, kid: &str, publish: bool) {
        self.lock()
            .keys
            .insert(0, (SigningKey::generate(kid), publish));
    }

    /// Every request answers 503 while `down`.
    pub fn set_down(&self, down: bool) {
        self.lock().down = down;
    }

    /// Every request answers `302 Location: <url>`.
    pub fn set_redirect(&self, url: Option<&str>) {
        self.lock().redirect_to = url.map(str::to_owned);
    }

    /// Every answer waits this long first.
    pub fn set_delay(&self, delay: Duration) {
        self.lock().delay = delay;
    }

    /// Request targets (path and query) in arrival order.
    pub fn requests(&self) -> Vec<String> {
        self.lock().requests.clone()
    }

    /// Requests to `path`, query ignored.
    pub fn count(&self, path: &str) -> usize {
        self.lock()
            .requests
            .iter()
            .filter(|t| t.split('?').next() == Some(path))
            .count()
    }

    fn lock(&self) -> MutexGuard<'_, Shared> {
        self.shared.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Drop for FakePi {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_secs() as i64
}

struct Request {
    method: String,
    target: String,
    authorization: Option<String>,
}

async fn answer(mut socket: TcpStream, shared: Arc<Mutex<Shared>>) {
    // Requests are bodiless GETs: the head ends at the first blank line.
    let mut head = Vec::new();
    let mut chunk = [0u8; 1024];
    while !head.windows(4).any(|w| w == b"\r\n\r\n") {
        match socket.read(&mut chunk).await {
            Ok(0) | Err(_) => return,
            Ok(n) => head.extend_from_slice(&chunk[..n]),
        }
        if head.len() > MAX_HEAD_BYTES {
            return;
        }
    }
    let Some(request) = parse(&head) else {
        return;
    };
    let (delay, response) = {
        let mut shared = shared.lock().unwrap_or_else(PoisonError::into_inner);
        shared.requests.push(request.target.clone());
        (shared.delay, respond(&shared, &request))
    };
    if !delay.is_zero() {
        tokio::time::sleep(delay).await;
    }
    let _ = socket.write_all(response.as_bytes()).await;
    let _ = socket.shutdown().await;
}

fn parse(head: &[u8]) -> Option<Request> {
    let text = std::str::from_utf8(head).ok()?;
    let mut lines = text.split("\r\n");
    let mut start = lines.next()?.split(' ');
    let method = start.next()?.to_owned();
    let target = start.next()?.to_owned();
    let authorization = lines
        .filter_map(|l| l.split_once(':'))
        .find(|(name, _)| name.trim().eq_ignore_ascii_case("authorization"))
        .map(|(_, value)| value.trim().to_owned());
    Some(Request {
        method,
        target,
        authorization,
    })
}

fn respond(shared: &Shared, request: &Request) -> String {
    if let Some(url) = &shared.redirect_to {
        return format!(
            "HTTP/1.1 302 Found\r\nlocation: {url}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
        );
    }
    if shared.down {
        return http(503, &json!({ "ok": false, "message": "down" }));
    }
    let path = request.target.split('?').next().unwrap_or_default();
    match (request.method.as_str(), path) {
        ("GET", "/id/jwks.json") => {
            let keys: Vec<Value> = shared
                .keys
                .iter()
                .filter(|(_, published)| *published)
                .map(|(key, _)| key.jwk())
                .collect();
            http(200, &json!({ "keys": keys }))
        }
        ("GET", "/id/userinfo") => userinfo(shared, request.authorization.as_deref()),
        _ => http(404, &json!({ "ok": false, "message": "not found" })),
    }
}

/// PI's `/id/userinfo`: a valid token of an active account gets its profile claims.
fn userinfo(shared: &Shared, authorization: Option<&str>) -> String {
    let claims = authorization
        .and_then(|a| a.strip_prefix("Bearer "))
        .and_then(|token| verify(shared, token));
    let user = claims.as_ref().and_then(|c| {
        let sub = c["sub"].as_str()?;
        shared
            .users
            .get(sub)
            .filter(|u| !u.locked)
            .map(|u| (sub, u))
    });
    let Some((sub, user)) = user else {
        return http(401, &json!({ "error": "invalid_token" }));
    };
    let scope = claims
        .as_ref()
        .and_then(|c| c["scope"].as_str())
        .unwrap_or_default();
    let mut body = json!({ "sub": sub });
    if scope.split(' ').any(|s| s == "profile") {
        body["name"] = json!(user.name);
        body["locale"] = json!("vi-VN");
        body["country"] = json!("VN");
    }
    if scope.split(' ').any(|s| s == "email")
        && let Some((email, verified)) = &user.email
    {
        body["email"] = json!(email);
        body["email_verified"] = json!(verified);
    }
    http(200, &body)
}

/// Signature and expiry against the published keys, as PI's `Jwt::verify`.
fn verify(shared: &Shared, token: &str) -> Option<Value> {
    let (input, signature) = token.rsplit_once('.')?;
    let (header, payload) = input.split_once('.')?;
    let header: Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(header).ok()?).ok()?;
    if header["alg"] != "EdDSA" || header["typ"] != "JWT" {
        return None;
    }
    let kid = header["kid"].as_str()?;
    let (key, _) = shared
        .keys
        .iter()
        .find(|(k, published)| *published && k.kid() == kid)?;
    let signature = URL_SAFE_NO_PAD.decode(signature).ok()?;
    if !key.verifies(input.as_bytes(), &signature) {
        return None;
    }
    let claims: Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload).ok()?).ok()?;
    (claims["exp"].as_i64()? + 30 >= unix_now()).then_some(claims)
}

fn http(status: u16, body: &Value) -> String {
    let reason = match status {
        200 => "OK",
        401 => "Unauthorized",
        404 => "Not Found",
        _ => "Service Unavailable",
    };
    let body = body.to_string();
    format!(
        "HTTP/1.1 {status} {reason}\r\ncontent-type: application/json\r\ncache-control: public, max-age=300\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    )
}
