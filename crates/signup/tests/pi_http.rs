//! `HttpPiRegistrar` against a local stand-in that answers as PI's `ServiceAccountsController::register`.
use std::{
    sync::{Arc, Mutex, PoisonError},
    time::Duration,
};

use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::post,
};
use office_signup::{
    HttpPiRegistrar, PiRegistrar, PiRegistrarConfig, RegisterError, RegisterRequest, Registered,
    ServiceKey,
};
use serde_json::{Value, json};

const KEY: &str = "office-test-service-key";

/// The service key header PI received, and the body.
type Seen = (Option<String>, Value);

#[derive(Clone, Default)]
struct FakePi {
    answer: Arc<Mutex<(u16, Value)>>,
    seen: Arc<Mutex<Vec<Seen>>>,
}

async fn register(
    State(pi): State<FakePi>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> (StatusCode, Json<Value>) {
    let key = headers
        .get("x-pi-service-key")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    pi.seen
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push((key.clone(), body));
    if key.as_deref() != Some(KEY) {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"ok": false, "message": "Thiếu hoặc sai khoá dịch vụ."})),
        );
    }
    let (status, body) = pi
        .answer
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    (StatusCode::from_u16(status).expect("status"), Json(body))
}

async fn start(answer: (u16, Value)) -> (FakePi, String) {
    let pi = FakePi {
        answer: Arc::new(Mutex::new(answer)),
        ..FakePi::default()
    };
    let app = Router::new()
        .route("/api/v1/service/accounts/register", post(register))
        .with_state(pi.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let issuer = format!("http://{}", listener.local_addr().expect("address"));
    tokio::spawn(async move { axum::serve(listener, app).await.expect("serve") });
    (pi, issuer)
}

fn registrar(issuer: &str, key: &str) -> HttpPiRegistrar {
    HttpPiRegistrar::new(PiRegistrarConfig {
        issuer: format!("{issuer}/"),
        service_key: ServiceKey::new(key),
        timeout: Duration::from_secs(2),
    })
    .expect("registrar")
}

fn request() -> RegisterRequest<'static> {
    RegisterRequest::new(
        "0192f7a0-0000-7000-8000-000000000001".to_owned(),
        "an@erpcons.test",
        "Nguyễn Văn An",
        "$2y$12$abcdefghijklmnopqrstuuabcdefghijklmnopqrstuvwxyz01234",
    )
}

async fn outcome(answer: (u16, Value)) -> Result<Registered, RegisterError> {
    let (_pi, issuer) = start(answer).await;
    registrar(&issuer, KEY).register(&request()).await
}

#[tokio::test]
async fn sends_the_key_and_the_hash_and_reads_a_created_account() {
    let (pi, issuer) = start((
        201,
        json!({"ok": true, "account": {}, "created": true, "passwordAdopted": true}),
    ))
    .await;
    let result = registrar(&issuer, KEY).register(&request()).await;
    assert_eq!(
        result,
        Ok(Registered::Created {
            password_adopted: true
        })
    );
    let seen = pi.seen.lock().expect("seen").clone();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].0.as_deref(), Some(KEY));
    assert_eq!(
        seen[0].1,
        json!({"localRef": "0192f7a0-0000-7000-8000-000000000001", "email": "an@erpcons.test",
            "displayName": "Nguyễn Văn An", "source": "office_signup",
            "passwordHash": "$2y$12$abcdefghijklmnopqrstuuabcdefghijklmnopqrstuvwxyz01234"})
    );
}

#[tokio::test]
async fn an_existing_or_linked_account_is_registered_too() {
    assert_eq!(
        outcome((
            200,
            json!({"ok": true, "created": false, "passwordAdopted": false})
        ))
        .await,
        Ok(Registered::Existing {
            password_adopted: false
        })
    );
    assert_eq!(
        outcome((
            409,
            json!({"ok": false, "message": "…", "code": "ACCOUNT_LINKED_ELSEWHERE"})
        ))
        .await,
        Ok(Registered::LinkedElsewhere)
    );
}

#[tokio::test]
async fn refusals_are_final_and_outages_are_retried() {
    assert_eq!(
        outcome((409, json!({"ok": false, "code": "ACCOUNT_LOCKED"}))).await,
        Err(RegisterError::Refused("account locked"))
    );
    assert_eq!(
        outcome((409, json!({"ok": false, "code": "LOCAL_REF_TAKEN"}))).await,
        Err(RegisterError::Refused("local ref taken"))
    );
    assert_eq!(
        outcome((422, json!({"ok": false, "errors": {"email": "x"}}))).await,
        Err(RegisterError::Refused("invalid"))
    );
    for status in [500, 502, 503, 404, 429] {
        assert_eq!(
            outcome((status, json!({"ok": false}))).await,
            Err(RegisterError::Transient),
            "{status}"
        );
    }
    // A 200 that is not PI's object (a proxy page) is not taken as success.
    assert_eq!(
        outcome((200, json!("<html>"))).await,
        Err(RegisterError::Transient)
    );
}

#[tokio::test]
async fn a_wrong_key_or_an_unreachable_pi_is_retried() {
    let (_pi, issuer) = start((201, json!({"ok": true, "created": true}))).await;
    assert_eq!(
        registrar(&issuer, "wrong-key").register(&request()).await,
        Err(RegisterError::Transient)
    );
    let closed = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        listener.local_addr().expect("address")
    };
    assert_eq!(
        registrar(&format!("http://{closed}"), KEY)
            .register(&request())
            .await,
        Err(RegisterError::Transient)
    );
}
