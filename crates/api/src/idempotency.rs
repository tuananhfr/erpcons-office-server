//! `Idempotency-Key` on the create and purge operations: the first response is kept 24 h and
//! replayed for a repeat with the same body; a repeat with another body is refused.
use axum::{
    Json,
    http::{HeaderValue, StatusCode, header::ETAG},
    response::{IntoResponse, Response},
};
use office_db::{actors, idempotency, idempotency::Claim};
use office_identity::Actor;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tracing::error;

use crate::{ApiError, AppState, ErrorCode, extract::IdempotencyKey};

const TTL_SECS: i64 = 24 * 3600;
/// A claim unfinished this long belongs to a request that died (the work it guards is one
/// transaction; uploads are stored before the claim).
const STALE_SECS: i64 = 60;

/// A success response as kept for replays.
#[derive(Serialize, Deserialize)]
pub(crate) struct Stored {
    status: u16,
    etag: Option<String>,
    body: Value,
}

impl Stored {
    pub(crate) fn new(status: StatusCode, etag: Option<&str>, body: impl Serialize) -> Self {
        Self {
            status: status.as_u16(),
            etag: etag.map(str::to_owned),
            body: serde_json::to_value(body).unwrap_or(Value::Null),
        }
    }

    /// A response without a body (`204`).
    pub(crate) fn empty(status: StatusCode) -> Self {
        Self {
            status: status.as_u16(),
            etag: None,
            body: Value::Null,
        }
    }
}

impl IntoResponse for Stored {
    fn into_response(self) -> Response {
        let status = StatusCode::from_u16(self.status).unwrap_or(StatusCode::OK);
        let mut response = if status == StatusCode::NO_CONTENT {
            status.into_response()
        } else {
            (status, Json(self.body)).into_response()
        };
        if let Some(etag) = self.etag.and_then(|e| HeaderValue::from_str(&e).ok()) {
            response.headers_mut().insert(ETAG, etag);
        }
        response
    }
}

/// Hash of what makes two requests "the same": the operation and its canonical input.
pub(crate) fn request_hash(operation: &str, input: impl Serialize) -> String {
    let canonical = serde_json::to_string(&input).unwrap_or_default();
    hex::encode(Sha256::digest(format!("{operation}\n{canonical}")))
}

/// Runs `op` once per `(actor, key)`; repeats get the stored response. A failed `op` releases the
/// key, so the client may retry with it.
pub(crate) async fn run<F, Fut>(
    state: &AppState,
    actor: &Actor,
    key: &IdempotencyKey,
    hash: &str,
    op: F,
) -> Result<Response, ApiError>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<Stored, ApiError>>,
{
    // The key row references the actor.
    actors::upsert_seen(&state.db, &actor.actor_id, &actor.display_name)
        .await
        .map_err(ApiError::database)?;
    let claim = idempotency::claim(
        &state.db,
        &actor.actor_id,
        &key.0,
        hash,
        TTL_SECS,
        STALE_SECS,
    )
    .await
    .map_err(ApiError::database)?;
    match claim {
        Claim::Existing { request_hash, .. } if request_hash != hash => {
            Err(ErrorCode::IdempotencyMismatch.into())
        }
        Claim::Existing { response: None, .. } => {
            Err(ApiError::retry_after(ErrorCode::Unavailable, 1))
        }
        Claim::Existing {
            response: Some(json),
            ..
        } => {
            let stored: Stored = serde_json::from_str(&json).map_err(|_| {
                error!("stored idempotent response is unreadable");
                ApiError::new(ErrorCode::Unavailable)
            })?;
            Ok(stored.into_response())
        }
        Claim::New => match op().await {
            Ok(stored) => {
                let json = serde_json::to_string(&stored).unwrap_or_default();
                // The work is committed; failing now would make the client repeat it.
                if let Err(e) =
                    idempotency::complete(&state.db, &actor.actor_id, &key.0, &json).await
                {
                    error!(error = %e, "storing the idempotent response failed");
                }
                Ok(stored.into_response())
            }
            Err(e) => {
                if let Err(db) = idempotency::release(&state.db, &actor.actor_id, &key.0).await {
                    error!(error = %db, "releasing the idempotency key failed");
                }
                Err(e)
            }
        },
    }
}
