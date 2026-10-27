use axum::{
    Extension, Json,
    extract::State,
    response::{IntoResponse, Response},
};

use super::respond::caller;
use crate::{
    ApiError, AppState, Authenticated, RequestId, dto::EntriesDto, extract::ApiQuery,
    requests::PageQuery,
};

pub(crate) async fn list(
    State(state): State<AppState>,
    Extension(rid): Extension<RequestId>,
    Authenticated(actor): Authenticated,
    ApiQuery(query): ApiQuery<PageQuery>,
) -> Result<Response, ApiError> {
    let page = query.page()?;
    let trash = state.docs.list_trash(&caller(&actor, &rid), &page).await?;
    Ok(Json(EntriesDto::from(&trash)).into_response())
}
