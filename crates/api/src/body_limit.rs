use axum::{
    extract::{Request, State},
    http::header::CONTENT_LENGTH,
    middleware::Next,
    response::{IntoResponse, Response},
};

use crate::{ApiError, AppState, ErrorCode};

/// Refuses a declared oversized body before reading any of it. Bodies without a length are cut
/// off while being read by `DefaultBodyLimit`.
pub(crate) async fn reject_oversized(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    let declared = request
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());
    if declared.is_some_and(|len| len > state.config.max_body_bytes) {
        return ApiError::new(ErrorCode::TooLarge).into_response();
    }
    next.run(request).await
}
