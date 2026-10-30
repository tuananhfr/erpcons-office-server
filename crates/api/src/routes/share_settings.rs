use axum::{
    Extension, Json,
    extract::State,
    response::{IntoResponse, Response},
};
use office_documents::ShareSettings;
use uuid::Uuid;

use super::respond::caller;
use crate::{
    ApiError, AppState, Authenticated, RequestId,
    dto::ShareSettingsDto,
    extract::{ApiJson, ApiPath},
    requests::PutShareSettings,
};

pub(crate) async fn put(
    State(state): State<AppState>,
    Extension(rid): Extension<RequestId>,
    Authenticated(actor): Authenticated,
    ApiPath(id): ApiPath<Uuid>,
    ApiJson(body): ApiJson<PutShareSettings>,
) -> Result<Response, ApiError> {
    let settings = ShareSettings {
        editors_can_share: body.editors_can_share,
        viewers_can_download: body.viewers_can_download,
    };
    let saved = state
        .docs
        .put_share_settings(&caller(&actor, &rid), id, settings)
        .await?;
    Ok(Json(ShareSettingsDto::from(saved)).into_response())
}
