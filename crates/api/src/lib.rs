//! HTTP gateway of the Office online tier: the `/api/v1` router with its shared pieces (error
//! body, request ids, bearer authentication, rate and body limits, audit, request tracing).
mod audit;
mod auth;
mod bearer;
mod body_limit;
mod client_ip;
mod config;
mod error;
mod rate_limit;
mod request_id;
mod routes;
mod state;
mod trace;

use axum::{
    Router,
    extract::DefaultBodyLimit,
    http::{HeaderValue, header::CACHE_CONTROL},
    middleware::{from_fn, from_fn_with_state},
    routing::get,
};
use tower_http::set_header::SetResponseHeaderLayer;

pub use auth::Authenticated;
pub use config::{ApiConfig, BODY_OVERHEAD_BYTES, RateConfig};
pub use error::{ApiError, ErrorCode};
pub use request_id::RequestId;
pub use state::AppState;

/// The whole HTTP surface. Layers run outermost first: request id, tracing, per-IP limit, body limit.
pub fn router(state: AppState) -> Router {
    let body_limit = usize::try_from(state.config.max_body_bytes).unwrap_or(usize::MAX);
    let api = Router::new()
        .route("/health", get(routes::health::get))
        .route("/me", get(routes::me::get));
    Router::new()
        .nest("/api/v1", api)
        .fallback(routes::not_found)
        .layer(DefaultBodyLimit::max(body_limit))
        .layer(from_fn_with_state(
            state.clone(),
            body_limit::reject_oversized,
        ))
        .layer(from_fn_with_state(state.clone(), rate_limit::per_ip))
        .layer(SetResponseHeaderLayer::if_not_present(
            CACHE_CONTROL,
            HeaderValue::from_static("no-store"),
        ))
        .layer(trace::layer())
        .layer(from_fn(request_id::assign))
        .with_state(state)
}
