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
/// The live room's leader names the last room update the bytes hold in `X-Live-Seq`.
pub(crate) async fn put(
    State(state): State<AppState>,
    Extension(rid): Extension<RequestId>,
    Authenticated(actor): Authenticated,
    ApiPath(id): ApiPath<Uuid>,
    headers: HeaderMap,
    body: Body,
) -> Result<Response, ApiError> {
    let tags = if_match(&headers)?.ok_or(ErrorCode::PreconditionRequired)?;
    let live_seq = live_seq(&headers)?;
    let _slot = upload_slot(&state, &actor.actor_id)?;
    let reader = StreamReader::new(body.into_data_stream().map_err(io::Error::other));
    let doc = state
        .docs
        .save_content(&caller(&actor, &rid), id, &tags, live_seq, Box::pin(reader))
        .await?;
    Ok(respond::document(StatusCode::OK, &doc))
}

/// `X-Live-Seq`: one header of decimal digits.
fn live_seq(headers: &HeaderMap) -> Result<Option<u64>, ApiError> {
    let mut values = headers.get_all("x-live-seq").iter();
    let Some(value) = values.next() else {
        return Ok(None);
    };
    let text = value.to_str().map_err(|_| ErrorCode::InvalidRequest)?;
    if values.next().is_some() || text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return Err(ErrorCode::InvalidRequest.into());
    }
    text.parse()
        .map(Some)
        .map_err(|_| ErrorCode::InvalidRequest.into())
}
