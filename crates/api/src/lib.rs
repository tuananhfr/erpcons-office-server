//! HTTP gateway of the Office online tier: the `/api/v1` router with its shared pieces (error
//! body, request ids, bearer authentication, rate and body limits, audit, request tracing).
mod audit;
mod auth;
mod bearer;
mod body_limit;
mod client_ip;
mod config;
mod dto;
mod error;
mod extract;
mod idempotency;
mod rate_limit;
mod request_id;
mod requests;
mod routes;
mod state;
mod trace;
mod trusted_proxies;

use axum::{
    Router,
    extract::DefaultBodyLimit,
    http::{HeaderValue, header::CACHE_CONTROL},
    middleware::{from_fn, from_fn_with_state},
    routing::{get, post},
};
use tower_http::set_header::SetResponseHeaderLayer;

pub use auth::Authenticated;
pub use config::{ApiConfig, BODY_OVERHEAD_BYTES, JSON_BODY_BYTES, RateConfig};
pub use error::{ApiError, ErrorCode};
pub use request_id::RequestId;
pub use state::AppState;
pub use trusted_proxies::{InvalidIpNet, IpNet, TrustedProxies};

/// The whole HTTP surface. Layers run outermost first: request id, tracing, per-IP limit, body limit.
/// JSON bodies stop at [`JSON_BODY_BYTES`]; only the upload and save routes take more, which they
/// stream and hold to the per-file limit themselves.
pub fn router(state: AppState) -> Router {
    let json_limit = JSON_BODY_BYTES.min(state.config.max_body_bytes);
    let json_limit = usize::try_from(json_limit).unwrap_or(usize::MAX);
    let json = Router::new()
        .route("/health", get(routes::health::get))
        .route("/me", get(routes::me::get))
        .route("/folders", post(routes::folders::create))
        .route(
            "/folders/{folder_id}",
            get(routes::folders::get)
                .patch(routes::folders::update)
                .delete(routes::folders::trash),
        )
        .route(
            "/folders/{folder_id}/children",
            get(routes::folders::children),
        )
        .route(
            "/folders/{folder_id}/restore",
            post(routes::folders::restore),
        )
        .route("/folders/{folder_id}/purge", post(routes::folders::purge))
        .route(
            "/documents/{document_id}",
            get(routes::documents::get)
                .patch(routes::documents::update)
                .delete(routes::documents::trash),
        )
        .route(
            "/documents/{document_id}/restore",
            post(routes::documents::restore),
        )
        .route(
            "/documents/{document_id}/purge",
            post(routes::documents::purge),
        )
        .route(
            "/documents/{document_id}/versions",
            get(routes::documents::versions),
        )
        .route("/trash", get(routes::trash::list))
        .route_layer(from_fn_with_state(
            state.clone(),
            body_limit::reject_oversized_json,
        ));
    // The blank-document JSON body of `POST /documents` is still held to `json_limit` when read.
    let streamed = Router::new()
        .route("/documents", post(routes::upload::create))
        .route(
            "/documents/{document_id}/content",
            get(routes::content::get).put(routes::content::put),
        );
    let api = json
        .merge(streamed)
        .method_not_allowed_fallback(routes::method_not_allowed);
    Router::new()
        .nest("/api/v1", api)
        .fallback(routes::not_found)
        .layer(DefaultBodyLimit::max(json_limit))
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
