use axum::{
    Extension, Json,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use office_documents::DocumentPatch;
use uuid::Uuid;

use super::respond::{self, caller};
use crate::{
    ApiError, AppState, Authenticated, RequestId,
    dto::{DocumentDto, VersionsDto},
    extract::{ApiJson, ApiPath, ApiQuery, IdempotencyKey},
    idempotency::{self, Stored},
    requests::{PageQuery, UpdateDocument},
};

pub(crate) async fn get(
    State(state): State<AppState>,
    Extension(rid): Extension<RequestId>,
    Authenticated(actor): Authenticated,
    ApiPath(id): ApiPath<Uuid>,
) -> Result<Response, ApiError> {
    let doc = state.docs.get_document(&caller(&actor, &rid), id).await?;
    Ok(respond::document(StatusCode::OK, &doc))
}

pub(crate) async fn update(
    State(state): State<AppState>,
    Extension(rid): Extension<RequestId>,
    Authenticated(actor): Authenticated,
    ApiPath(id): ApiPath<Uuid>,
    ApiJson(body): ApiJson<UpdateDocument>,
) -> Result<Response, ApiError> {
    let patch = DocumentPatch {
        title: body.title,
        folder_id: body.folder_id,
    };
    let doc = state
        .docs
        .update_document(&caller(&actor, &rid), id, patch)
        .await?;
    Ok(Json(DocumentDto::from(&doc)).into_response())
}

pub(crate) async fn trash(
    State(state): State<AppState>,
    Extension(rid): Extension<RequestId>,
    Authenticated(actor): Authenticated,
    ApiPath(id): ApiPath<Uuid>,
) -> Result<StatusCode, ApiError> {
    state.docs.trash_document(&caller(&actor, &rid), id).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub(crate) async fn restore(
    State(state): State<AppState>,
    Extension(rid): Extension<RequestId>,
    Authenticated(actor): Authenticated,
    ApiPath(id): ApiPath<Uuid>,
) -> Result<Response, ApiError> {
    let doc = state
        .docs
        .restore_document(&caller(&actor, &rid), id)
        .await?;
    Ok(Json(DocumentDto::from(&doc)).into_response())
}

/// Deletes a trashed document for good.
pub(crate) async fn purge(
    State(state): State<AppState>,
    Extension(rid): Extension<RequestId>,
    Authenticated(actor): Authenticated,
    ApiPath(id): ApiPath<Uuid>,
    key: IdempotencyKey,
) -> Result<Response, ApiError> {
    let hash = idempotency::request_hash("document.purge", id);
    let (docs, c) = (&state.docs, caller(&actor, &rid));
    idempotency::run(&state, &actor, &key, &hash, move || async move {
        docs.purge_document(&c, id).await?;
        Ok(Stored::empty(StatusCode::NO_CONTENT))
    })
    .await
}

pub(crate) async fn versions(
    State(state): State<AppState>,
    Extension(rid): Extension<RequestId>,
    Authenticated(actor): Authenticated,
    ApiPath(id): ApiPath<Uuid>,
    ApiQuery(query): ApiQuery<PageQuery>,
) -> Result<Response, ApiError> {
    let page = query.page()?;
    let versions = state
        .docs
        .list_versions(&caller(&actor, &rid), id, &page)
        .await?;
    Ok(Json(VersionsDto::from(&versions)).into_response())
}
