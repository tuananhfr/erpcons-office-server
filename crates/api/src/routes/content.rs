use std::io;

use axum::{
    Extension,
    body::Body,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::Response,
};
use futures_util::TryStreamExt;
use tokio_util::io::StreamReader;
use uuid::Uuid;

use super::respond::{self, caller};
use crate::{
    ApiError, AppState, Authenticated, ErrorCode, RequestId,
    extract::{ApiPath, if_match},
    rate_limit::upload_slot,
};

/// Streams the current version's bytes.
pub(crate) async fn get(
    State(state): State<AppState>,
    Extension(rid): Extension<RequestId>,
    Authenticated(actor): Authenticated,
    ApiPath(id): ApiPath<Uuid>,
) -> Result<Response, ApiError> {
    let content = state.docs.open_content(&caller(&actor, &rid), id).await?;
    Ok(respond::content(content))
}

/// Saves a new version; the body streams to storage, it is never held in memory whole. A repeat
/// of the save that made the current version answers that version (see `replay_or_conflict`).
pub(crate) async fn put(
    State(state): State<AppState>,
    Extension(rid): Extension<RequestId>,
    Authenticated(actor): Authenticated,
    ApiPath(id): ApiPath<Uuid>,
    headers: HeaderMap,
    body: Body,
) -> Result<Response, ApiError> {
    let tags = if_match(&headers)?.ok_or(ErrorCode::PreconditionRequired)?;
    let _slot = upload_slot(&state, &actor.actor_id)?;
    let reader = StreamReader::new(body.into_data_stream().map_err(io::Error::other));
    let doc = state
        .docs
        .save_content(&caller(&actor, &rid), id, &tags, Box::pin(reader))
        .await?;
    Ok(respond::document(StatusCode::OK, &doc))
}
