use axum::{Extension, extract::State, http::HeaderMap, http::StatusCode, response::Response};
use uuid::Uuid;

use super::respond::{self, caller};
use crate::{
    ApiError, AppState, Authenticated, ErrorCode, RequestId,
    extract::{ApiPath, if_match},
};

fn seq_of(seq: i32) -> Result<i32, ApiError> {
    if seq >= 1 {
        Ok(seq)
    } else {
        Err(ErrorCode::InvalidRequest.into())
    }
}

/// Streams the bytes of one version.
pub(crate) async fn content(
    State(state): State<AppState>,
    Extension(rid): Extension<RequestId>,
    Authenticated(actor): Authenticated,
    ApiPath((id, seq)): ApiPath<(Uuid, i32)>,
) -> Result<Response, ApiError> {
    let content = state
        .docs
        .version_content(&caller(&actor, &rid), id, seq_of(seq)?)
        .await?;
    Ok(respond::content(content))
}

/// Deletes an old version for good (owner only).
pub(crate) async fn remove(
    State(state): State<AppState>,
    Extension(rid): Extension<RequestId>,
    Authenticated(actor): Authenticated,
    ApiPath((id, seq)): ApiPath<(Uuid, i32)>,
) -> Result<StatusCode, ApiError> {
    state
        .docs
        .delete_version(&caller(&actor, &rid), id, seq_of(seq)?)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Makes an old version current again, as a new version.
pub(crate) async fn restore(
    State(state): State<AppState>,
    Extension(rid): Extension<RequestId>,
    Authenticated(actor): Authenticated,
    ApiPath((id, seq)): ApiPath<(Uuid, i32)>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let seq = seq_of(seq)?;
    let tags = if_match(&headers)?.ok_or(ErrorCode::PreconditionRequired)?;
    let doc = state
        .docs
        .restore_version(&caller(&actor, &rid), id, seq, &tags)
        .await?;
    Ok(respond::document(StatusCode::OK, &doc))
}
