use axum::{
    Extension, Json,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use office_documents::{FolderPatch, PageRequest};
use uuid::Uuid;

use super::respond::caller;
use crate::{
    ApiError, AppState, Authenticated, ErrorCode, RequestId,
    dto::{EntriesDto, FolderDto},
    extract::{ApiJson, ApiPath, ApiQuery, IdempotencyKey},
    idempotency::{self, Stored},
    requests::{CreateFolder, PageQuery, UpdateFolder},
};

/// `root` or a folder id.
fn parent_of(path: &str) -> Result<Option<Uuid>, ApiError> {
    if path == "root" {
        return Ok(None);
    }
    Uuid::parse_str(path)
        .map(Some)
        .map_err(|_| ErrorCode::InvalidRequest.into())
}

pub(crate) async fn children(
    State(state): State<AppState>,
    Extension(rid): Extension<RequestId>,
    Authenticated(actor): Authenticated,
    ApiPath(folder): ApiPath<String>,
    ApiQuery(query): ApiQuery<PageQuery>,
) -> Result<Response, ApiError> {
    let folder = parent_of(&folder)?;
    let page: PageRequest = query.page()?;
    let listing = state
        .docs
        .list_children(&caller(&actor, &rid), folder, &page)
        .await?;
    Ok(Json(EntriesDto::from(&listing)).into_response())
}

pub(crate) async fn create(
    State(state): State<AppState>,
    Extension(rid): Extension<RequestId>,
    Authenticated(actor): Authenticated,
    key: IdempotencyKey,
    ApiJson(body): ApiJson<CreateFolder>,
) -> Result<Response, ApiError> {
    let hash = idempotency::request_hash("folder.create", &body);
    let (docs, c, body) = (&state.docs, caller(&actor, &rid), &body);
    idempotency::run(&state, &actor, &key, &hash, move || async move {
        let folder = docs.create_folder(&c, body.parent_id, &body.name).await?;
        Ok(Stored::new(
            StatusCode::CREATED,
            None,
            FolderDto::from(&folder),
        ))
    })
    .await
}

pub(crate) async fn update(
    State(state): State<AppState>,
    Extension(rid): Extension<RequestId>,
    Authenticated(actor): Authenticated,
    ApiPath(id): ApiPath<Uuid>,
    ApiJson(body): ApiJson<UpdateFolder>,
) -> Result<Response, ApiError> {
    let patch = FolderPatch {
        name: body.name,
        parent_id: body.parent_id,
    };
    let folder = state
        .docs
        .update_folder(&caller(&actor, &rid), id, patch)
        .await?;
    Ok(Json(FolderDto::from(&folder)).into_response())
}

pub(crate) async fn trash(
    State(state): State<AppState>,
    Extension(rid): Extension<RequestId>,
    Authenticated(actor): Authenticated,
    ApiPath(id): ApiPath<Uuid>,
) -> Result<StatusCode, ApiError> {
    state.docs.trash_folder(&caller(&actor, &rid), id).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub(crate) async fn restore(
    State(state): State<AppState>,
    Extension(rid): Extension<RequestId>,
    Authenticated(actor): Authenticated,
    ApiPath(id): ApiPath<Uuid>,
) -> Result<Response, ApiError> {
    let folder = state.docs.restore_folder(&caller(&actor, &rid), id).await?;
    Ok(Json(FolderDto::from(&folder)).into_response())
}
