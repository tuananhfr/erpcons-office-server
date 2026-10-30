use axum::{
    Extension, Json,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use uuid::Uuid;

use super::respond::caller;
use crate::{
    ApiError, AppState, Authenticated, RequestId,
    dto::SharedDto,
    extract::{ApiPath, ApiQuery},
    requests::PageQuery,
};

/// "Shared with me".
pub(crate) async fn list(
    State(state): State<AppState>,
    Extension(rid): Extension<RequestId>,
    Authenticated(actor): Authenticated,
    ApiQuery(query): ApiQuery<PageQuery>,
) -> Result<Response, ApiError> {
    let page = query.page()?;
    let shared = state.docs.list_shared(&caller(&actor, &rid), &page).await?;
    Ok(Json(SharedDto::from(&shared)).into_response())
}

pub(crate) async fn seen(
    State(state): State<AppState>,
    Extension(rid): Extension<RequestId>,
    Authenticated(actor): Authenticated,
    ApiPath(id): ApiPath<Uuid>,
) -> Result<StatusCode, ApiError> {
    state
        .docs
        .mark_shared_seen(&caller(&actor, &rid), id)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
