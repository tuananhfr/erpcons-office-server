use axum::{
    Extension, Json,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use office_documents::{AddPermissions, PermissionPatch};
use uuid::Uuid;

use super::respond::caller;
use crate::{
    ApiError, AppState, Authenticated, RequestId,
    dto::{AddedPermissionsDto, PermissionDto, PermissionsDto},
    extract::{ApiJson, ApiPath},
    requests::{self, grant_role},
};

pub(crate) async fn list(
    State(state): State<AppState>,
    Extension(rid): Extension<RequestId>,
    Authenticated(actor): Authenticated,
    ApiPath(id): ApiPath<Uuid>,
) -> Result<Response, ApiError> {
    let permissions = state
        .docs
        .list_permissions(&caller(&actor, &rid), id)
        .await?;
    Ok(Json(PermissionsDto::from(&permissions)).into_response())
}

pub(crate) async fn add(
    State(state): State<AppState>,
    Extension(rid): Extension<RequestId>,
    Authenticated(actor): Authenticated,
    ApiPath(id): ApiPath<Uuid>,
    ApiJson(body): ApiJson<requests::AddPermissions>,
) -> Result<Response, ApiError> {
    let request = AddPermissions {
        emails: body.emails,
        role: grant_role(&body.role)?,
        notify: body.notify,
        message: body.message,
    };
    let added = state
        .docs
        .add_permissions(&caller(&actor, &rid), id, request)
        .await?;
    Ok(Json(AddedPermissionsDto::from(&added)).into_response())
}

pub(crate) async fn update(
    State(state): State<AppState>,
    Extension(rid): Extension<RequestId>,
    Authenticated(actor): Authenticated,
    ApiPath((id, permission_id)): ApiPath<(Uuid, Uuid)>,
    ApiJson(body): ApiJson<requests::UpdatePermission>,
) -> Result<Response, ApiError> {
    let patch = PermissionPatch {
        role: body.role.as_deref().map(grant_role).transpose()?,
        expires_at: body.expires_at,
    };
    let permission = state
        .docs
        .update_permission(&caller(&actor, &rid), id, permission_id, patch)
        .await?;
    Ok(Json(PermissionDto::from(&permission)).into_response())
}

/// Removes a permission; anyone may remove their own.
pub(crate) async fn remove(
    State(state): State<AppState>,
    Extension(rid): Extension<RequestId>,
    Authenticated(actor): Authenticated,
    ApiPath((id, permission_id)): ApiPath<(Uuid, Uuid)>,
) -> Result<StatusCode, ApiError> {
    state
        .docs
        .remove_permission(&caller(&actor, &rid), id, permission_id)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
