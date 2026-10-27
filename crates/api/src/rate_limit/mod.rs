mod bucket;

use std::time::Instant;

use axum::{
    extract::{Request, State},
    middleware::Next,
    response::{IntoResponse, Response},
};

pub(crate) use bucket::KeyedLimiter;

use crate::{ApiError, AppState, client_ip::client_ip};

/// Per-address limit. It runs before authentication, so a flood never reaches PI or the database.
pub(crate) async fn per_ip(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    let ip = client_ip(&request, state.config.trust_forwarded_for);
    match state.ip_limiter.check(ip, Instant::now()) {
        Ok(()) => next.run(request).await,
        Err(retry_after) => ApiError::rate_limited(retry_after).into_response(),
    }
}
