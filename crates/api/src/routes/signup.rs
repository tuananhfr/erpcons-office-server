//! Self sign-up, without signing in. Answers never tell whether an address is known: every accepted
//! sign-up gets the same 202, whatever the address's state.
use std::time::Instant;

use axum::{Extension, Json, extract::State, http::StatusCode};
use office_signup::SignupRequest;
use serde::{Deserialize, Serialize};

use crate::{
    ApiError, AppState,
    extract::ApiJson,
    rate_limit::{ClientKey, KeyedLimiter},
};

fn check_rate(
    limiter: &KeyedLimiter<ClientKey>,
    client: Option<Extension<ClientKey>>,
) -> Result<(), ApiError> {
    let key = client.map_or(ClientKey(None), |Extension(k)| k);
    limiter
        .check(key, Instant::now())
        .map_err(ApiError::rate_limited)
}

#[derive(Serialize)]
pub(crate) struct Status {
    open: bool,
}

/// Whether this server takes sign-ups, so the page can say so before anyone types.
pub(crate) async fn status(State(state): State<AppState>) -> Json<Status> {
    Json(Status {
        open: state.signups.is_open(),
    })
}

#[derive(Serialize)]
pub(crate) struct Accepted {
    accepted: bool,
}

pub(crate) async fn create(
    State(state): State<AppState>,
    client: Option<Extension<ClientKey>>,
    ApiJson(request): ApiJson<SignupRequest>,
) -> Result<(StatusCode, Json<Accepted>), ApiError> {
    if !state.signups.is_open() {
        return Err(office_signup::SignupError::Closed.into());
    }
    check_rate(&state.signup_limiter, client)?;
    state.signups.submit(&request).await?;
    Ok((StatusCode::ACCEPTED, Json(Accepted { accepted: true })))
}

#[derive(Deserialize)]
pub(crate) struct VerifyRequest {
    #[serde(default)]
    token: String,
}

#[derive(Serialize)]
pub(crate) struct Verified {
    verified: bool,
}

pub(crate) async fn verify(
    State(state): State<AppState>,
    client: Option<Extension<ClientKey>>,
    ApiJson(request): ApiJson<VerifyRequest>,
) -> Result<Json<Verified>, ApiError> {
    check_rate(&state.signup_verify_limiter, client)?;
    state.signups.verify(&request.token).await?;
    Ok(Json(Verified { verified: true }))
}
