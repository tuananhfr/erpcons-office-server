mod bucket;
mod concurrency;
mod key;

use std::time::Instant;

use axum::{
    extract::{Request, State},
    middleware::Next,
    response::{IntoResponse, Response},
};

pub(crate) use bucket::KeyedLimiter;
pub(crate) use concurrency::{ConcurrencyLimiter, Slot};
pub(crate) use key::ClientKey;

use crate::{ApiError, AppState, client_ip::client_ip};

/// An upload holds its slot for as long as its body streams; a second is a fair wait.
const UPLOAD_RETRY_SECS: u64 = 1;

/// Per-address limit. It runs before authentication, so a flood never reaches PI or the database.
/// The key it uses is left on the request for the limits that come later.
pub(crate) async fn per_ip(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    let key = ClientKey::of(client_ip(&request, &state.config));
    match state.ip_limiter.check(key, Instant::now()) {
        Ok(()) => {
            request.extensions_mut().insert(key);
            next.run(request).await
        }
        Err(retry_after) => ApiError::rate_limited(retry_after).into_response(),
    }
}

/// One of the actor's concurrent upload and save slots; 429 while all are taken, so one account
/// cannot hold every connection and disk stream.
pub(crate) fn upload_slot(state: &AppState, actor_id: &str) -> Result<Slot, ApiError> {
    state
        .upload_slots
        .try_acquire(actor_id)
        .ok_or_else(|| ApiError::rate_limited(UPLOAD_RETRY_SECS))
}
