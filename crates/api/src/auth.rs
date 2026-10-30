use std::time::Instant;

use axum::{extract::FromRequestParts, http::request::Parts};
use office_db::audit::{ActorKind, AuditEvent, Outcome};
use office_identity::{Actor, VerifyError};
use tracing::error;

use crate::{
    ApiError, AppState, ErrorCode, RequestId, audit, bearer::bearer_token, rate_limit::ClientKey,
};

/// The verified caller. Extracting it checks the bearer token, then the per-actor rate limit.
pub struct Authenticated(pub Actor);

impl FromRequestParts<AppState> for Authenticated {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        // A missing or malformed header is refused unaudited: it costs nothing to send.
        let token = bearer_token(&parts.headers).ok_or(ErrorCode::Unauthenticated)?;
        let client = parts
            .extensions
            .get::<ClientKey>()
            .copied()
            .unwrap_or(ClientKey(None));
        let verdict = match state.identity.cached(token) {
            Some(verdict) => verdict,
            None => {
                // A miss costs a signature check and may fetch PI's keys or a display name; random
                // tokens must not turn that into a load on this server or a flood against PI.
                state
                    .verify_limiter
                    .check(client, Instant::now())
                    .map_err(ApiError::rate_limited)?;
                let verdict = state.identity.verify(token).await;
                if let Ok(actor) = &verdict {
                    sync_actor(state, actor).await;
                }
                verdict
            }
        };
        let actor = match verdict {
            Ok(actor) => actor,
            Err(e) => {
                let unavailable = e == VerifyError::Unavailable;
                if state
                    .denial_audit
                    .check((client, unavailable), Instant::now())
                    .is_ok()
                {
                    record_failure(state, parts.extensions.get::<RequestId>(), e).await;
                }
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

/// Records the actor and its verified email, and hands it the invitations waiting for that email.
/// Once per verification, not per request. A failure only delays that, so the request goes on.
async fn sync_actor(state: &AppState, actor: &Actor) {
    if let Err(e) = state.docs.sync_actor(actor).await {
        error!(error = %e, "syncing the actor failed");
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
