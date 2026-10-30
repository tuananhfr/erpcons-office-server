//! Turning a document's public view link on and off (signed in). Opening it is `public_links`.
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
    dto::LinkCreatedDto,
    extract::{ApiPath, OptionalJson},
    requests::EnableLink,
};

pub(crate) async fn enable(
    State(state): State<AppState>,
    Extension(rid): Extension<RequestId>,
    Authenticated(actor): Authenticated,
    ApiPath(id): ApiPath<Uuid>,
    OptionalJson(body): OptionalJson<EnableLink>,
) -> Result<Response, ApiError> {
    let link = state
        .docs
        .enable_link(&caller(&actor, &rid), id, body.expires_at)
        .await?;
    Ok(Json(LinkCreatedDto::from(&link)).into_response())
}

pub(crate) async fn disable(
    State(state): State<AppState>,
    Extension(rid): Extension<RequestId>,
    Authenticated(actor): Authenticated,
    ApiPath(id): ApiPath<Uuid>,
) -> Result<StatusCode, ApiError> {
    state.docs.disable_link(&caller(&actor, &rid), id).await?;
    Ok(StatusCode::NO_CONTENT)
}
