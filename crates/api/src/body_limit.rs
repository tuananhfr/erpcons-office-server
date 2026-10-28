use axum::{
    extract::{Request, State},
    http::header::CONTENT_LENGTH,
    middleware::Next,
    response::{IntoResponse, Response},
};

use crate::{ApiError, AppState, ErrorCode, JSON_BODY_BYTES};

/// Refuses a declared body over the largest any route takes, before reading any of it. Bodies
/// without a length are cut off while being read (`DefaultBodyLimit`, or the upload limits).
pub(crate) async fn reject_oversized(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    reject_over(state.config.max_body_bytes, request, next).await
}

/// The same for the routes whose body is JSON.
pub(crate) async fn reject_oversized_json(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    reject_over(
        JSON_BODY_BYTES.min(state.config.max_body_bytes),
        request,
        next,
    )
    .await
}

async fn reject_over(limit: u64, request: Request, next: Next) -> Response {
    let declared = request
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());
    if declared.is_some_and(|len| len > limit) {
        return ApiError::new(ErrorCode::TooLarge).into_response();
    }
    next.run(request).await
}
