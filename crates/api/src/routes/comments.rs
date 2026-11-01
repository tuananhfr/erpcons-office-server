//! Comment threads of a document (plan-m5). Writes share a per-actor rate on top of the usual ones.
use std::time::Instant;

use axum::{
    Extension, Json,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use office_documents::{Anchor, AnchorChange, Body, NewThread, ThreadStatus};
use office_identity::Actor;
use serde_json::{Map, Value};
use uuid::Uuid;

use super::respond::caller;
use crate::{
    ApiError, AppState, Authenticated, ErrorCode, RequestId,
    dto::{AnchorsUpdatedDto, CommentDto, ThreadDto, ThreadsDto},
    extract::{ApiJson, ApiPath, ApiQuery, IdempotencyKey},
    idempotency::{self, Stored},
    requests::{ChangeAnchors, CommentBody, CommentsQuery, CreateThread, PageQuery},
};

fn write_allowed(state: &AppState, actor: &Actor) -> Result<(), ApiError> {
    state
        .comment_limiter
        .check(actor.actor_id.clone(), Instant::now())
        .map_err(ApiError::rate_limited)
}

fn status_of(raw: &str) -> Result<ThreadStatus, ApiError> {
    ThreadStatus::parse(raw).ok_or_else(|| ErrorCode::InvalidRequest.into())
}

/// Compact JSON of an anchor object.
fn anchor_of(map: &Map<String, Value>) -> Result<Anchor, ApiError> {
    let json = serde_json::to_string(map).map_err(|_| ErrorCode::InvalidRequest)?;
    Ok(Anchor::parse(json)?)
}

pub(crate) async fn list(
    State(state): State<AppState>,
    Extension(rid): Extension<RequestId>,
    Authenticated(actor): Authenticated,
    ApiPath(id): ApiPath<Uuid>,
    ApiQuery(query): ApiQuery<CommentsQuery>,
) -> Result<Response, ApiError> {
    let status = query.status.as_deref().map(status_of).transpose()?;
    let page = PageQuery {
        cursor: query.cursor,
        limit: query.limit,
    }
    .page()?;
    let threads = state
        .docs
        .list_comments(&caller(&actor, &rid), id, status, &page)
        .await?;
    Ok(Json(ThreadsDto::from(&threads)).into_response())
}

pub(crate) async fn get(
    State(state): State<AppState>,
    Extension(rid): Extension<RequestId>,
    Authenticated(actor): Authenticated,
    ApiPath((id, thread_id)): ApiPath<(Uuid, Uuid)>,
) -> Result<Response, ApiError> {
    let thread = state
        .docs
        .get_comment_thread(&caller(&actor, &rid), id, thread_id)
        .await?;
    Ok(Json(ThreadDto::from(&thread)).into_response())
}

pub(crate) async fn create(
    State(state): State<AppState>,
    Extension(rid): Extension<RequestId>,
    Authenticated(actor): Authenticated,
    ApiPath(id): ApiPath<Uuid>,
    key: IdempotencyKey,
    ApiJson(body): ApiJson<CreateThread>,
) -> Result<Response, ApiError> {
    write_allowed(&state, &actor)?;
    let hash = idempotency::request_hash("comment.create", (id, &body));
    let new = NewThread {
        anchor: anchor_of(&body.anchor)?,
        region: body.region,
        version: body.version,
        body: Body::parse(body.body)?,
    };
    let (docs, c) = (&state.docs, caller(&actor, &rid));
    idempotency::run(&state, &actor, &key, &hash, move || async move {
        let thread = docs.create_comment_thread(&c, id, new).await?;
        Ok(Stored::new(
            StatusCode::CREATED,
            None,
            ThreadDto::from(&thread),
        ))
    })
    .await
}

pub(crate) async fn reply(
    State(state): State<AppState>,
    Extension(rid): Extension<RequestId>,
    Authenticated(actor): Authenticated,
    ApiPath((id, thread_id)): ApiPath<(Uuid, Uuid)>,
    key: IdempotencyKey,
    ApiJson(body): ApiJson<CommentBody>,
) -> Result<Response, ApiError> {
    write_allowed(&state, &actor)?;
    let hash = idempotency::request_hash("comment.reply", (id, thread_id, &body));
    let words = Body::parse(body.body)?;
    let (docs, c) = (&state.docs, caller(&actor, &rid));
    idempotency::run(&state, &actor, &key, &hash, move || async move {
        let comment = docs.reply_to_thread(&c, id, thread_id, words).await?;
        Ok(Stored::new(
            StatusCode::CREATED,
            None,
            CommentDto::from(&comment),
        ))
    })
    .await
}

pub(crate) async fn edit(
    State(state): State<AppState>,
    Extension(rid): Extension<RequestId>,
    Authenticated(actor): Authenticated,
    ApiPath((id, thread_id, comment_id)): ApiPath<(Uuid, Uuid, Uuid)>,
    ApiJson(body): ApiJson<CommentBody>,
) -> Result<Response, ApiError> {
    write_allowed(&state, &actor)?;
    let words = Body::parse(body.body)?;
    let comment = state
        .docs
        .edit_comment(&caller(&actor, &rid), id, thread_id, comment_id, words)
        .await?;
    Ok(Json(CommentDto::from(&comment)).into_response())
}

pub(crate) async fn delete_comment(
    State(state): State<AppState>,
    Extension(rid): Extension<RequestId>,
    Authenticated(actor): Authenticated,
    ApiPath((id, thread_id, comment_id)): ApiPath<(Uuid, Uuid, Uuid)>,
) -> Result<StatusCode, ApiError> {
    write_allowed(&state, &actor)?;
    state
        .docs
        .delete_comment(&caller(&actor, &rid), id, thread_id, comment_id)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

pub(crate) async fn resolve(
    State(state): State<AppState>,
    Extension(rid): Extension<RequestId>,
    Authenticated(actor): Authenticated,
    ApiPath((id, thread_id)): ApiPath<(Uuid, Uuid)>,
) -> Result<Response, ApiError> {
    write_allowed(&state, &actor)?;
    let thread = state
        .docs
        .resolve_thread(&caller(&actor, &rid), id, thread_id)
        .await?;
    Ok(Json(ThreadDto::from(&thread)).into_response())
}

pub(crate) async fn reopen(
    State(state): State<AppState>,
    Extension(rid): Extension<RequestId>,
    Authenticated(actor): Authenticated,
    ApiPath((id, thread_id)): ApiPath<(Uuid, Uuid)>,
) -> Result<Response, ApiError> {
    write_allowed(&state, &actor)?;
    let thread = state
        .docs
        .reopen_thread(&caller(&actor, &rid), id, thread_id)
        .await?;
    Ok(Json(ThreadDto::from(&thread)).into_response())
}

pub(crate) async fn delete_thread(
    State(state): State<AppState>,
    Extension(rid): Extension<RequestId>,
    Authenticated(actor): Authenticated,
    ApiPath((id, thread_id)): ApiPath<(Uuid, Uuid)>,
) -> Result<StatusCode, ApiError> {
    write_allowed(&state, &actor)?;
    state
        .docs
        .delete_thread(&caller(&actor, &rid), id, thread_id)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

pub(crate) async fn anchors(
    State(state): State<AppState>,
    Extension(rid): Extension<RequestId>,
    Authenticated(actor): Authenticated,
    ApiPath(id): ApiPath<Uuid>,
    ApiJson(body): ApiJson<ChangeAnchors>,
) -> Result<Response, ApiError> {
    write_allowed(&state, &actor)?;
    let changes = body
        .updates
        .into_iter()
        .map(|u| {
            Ok(AnchorChange {
                thread_id: u.thread_id,
                anchor: u.anchor.as_ref().map(anchor_of).transpose()?,
                region: u.region,
                status: u.status.as_deref().map(status_of).transpose()?,
            })
        })
        .collect::<Result<Vec<_>, ApiError>>()?;
    let updated = state
        .docs
        .change_anchors(&caller(&actor, &rid), id, changes)
        .await?;
    Ok(Json(AnchorsUpdatedDto { updated }).into_response())
}
