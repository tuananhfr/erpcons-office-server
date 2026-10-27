use axum::{Extension, Json, extract::State};
use office_db::{
    actors,
    audit::{ActorKind, AuditEvent, Outcome},
    quota,
};
use serde::Serialize;

use crate::{ApiError, AppState, Authenticated, RequestId, audit};

#[derive(Serialize)]
pub(crate) struct Me {
    actor_id: String,
    display_name: String,
    quota: Quota,
}

#[derive(Serialize)]
struct Quota {
    used_bytes: u64,
    limit_bytes: u64,
    max_file_bytes: u64,
}

pub(crate) async fn get(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    Authenticated(actor): Authenticated,
) -> Result<Json<Me>, ApiError> {
    actors::upsert_seen(&state.db, &actor.actor_id, &actor.display_name)
        .await
        .map_err(ApiError::database)?;
    let used = quota::used_bytes(&state.db, &actor.actor_id)
        .await
        .map_err(ApiError::database)?;
    let event = AuditEvent {
        actor_kind: ActorKind::User,
        actor_id: Some(&actor.actor_id),
        tenant_id: None,
        document_id: None,
        folder_id: None,
        action: "me.read",
        outcome: Outcome::Success,
        trace_id: request_id.as_str(),
    };
    audit::record(&state, &event).await;
    let limits = state.config.quota;
    Ok(Json(Me {
        actor_id: actor.actor_id,
        display_name: actor.display_name,
        quota: Quota {
            used_bytes: u64::try_from(used).unwrap_or(0),
            limit_bytes: limits.limit_bytes,
            max_file_bytes: limits.max_file_bytes,
        },
    }))
}
