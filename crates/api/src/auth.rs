use std::time::Instant;

use axum::{extract::FromRequestParts, http::request::Parts};
use office_db::audit::{ActorKind, AuditEvent, Outcome};
use office_identity::{Actor, VerifyError};

use crate::{ApiError, AppState, ErrorCode, RequestId, audit, bearer::bearer_token};

/// The verified caller. Extracting it checks the bearer token, then the per-actor rate limit.
pub struct Authenticated(pub Actor);

impl FromRequestParts<AppState> for Authenticated {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        let verdict = match bearer_token(&parts.headers) {
            Some(token) => state.identity.verify(token).await,
            None => Err(VerifyError::Unauthenticated),
        };
        let actor = match verdict {
            Ok(actor) => actor,
            Err(e) => {
                record_failure(state, parts.extensions.get::<RequestId>(), e).await;
                return Err(match e {
                    VerifyError::Unauthenticated => ErrorCode::Unauthenticated,
                    VerifyError::Unavailable => ErrorCode::Unavailable,
                }
                .into());
            }
        };
        state
            .actor_limiter
            .check(actor.actor_id.clone(), Instant::now())
            .map_err(ApiError::rate_limited)?;
        Ok(Self(actor))
    }
}

async fn record_failure(state: &AppState, request_id: Option<&RequestId>, error: VerifyError) {
    let event = AuditEvent {
        actor_kind: ActorKind::User,
        actor_id: None,
        tenant_id: None,
        document_id: None,
        folder_id: None,
        action: "auth.verify",
        outcome: match error {
            VerifyError::Unauthenticated => Outcome::Denied,
            VerifyError::Unavailable => Outcome::Error,
        },
        trace_id: request_id.map_or("", RequestId::as_str),
    };
    audit::record(state, &event).await;
}
