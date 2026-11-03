//! Who reads through a route open to link holders: the link holder when `X-Link-Token` is sent
//! (the token alone decides, whoever is signed in), else the bearer's actor.
use axum::{extract::FromRequestParts, http::request::Parts};
use office_documents::LinkHash;

use super::public_links;
use crate::{ApiError, AppState, Authenticated, ErrorCode, rate_limit::ClientKey};

pub(crate) enum LinkOrActor {
    Actor(Authenticated),
    Link(LinkHash),
}

impl FromRequestParts<AppState> for LinkOrActor {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        if parts.headers.contains_key(public_links::TOKEN_HEADER) {
            public_links::check_rate(state, parts.extensions.get::<ClientKey>().copied())?;
            let token = public_links::token(&parts.headers)?;
            // A malformed token is as unknown as a wrong one.
            let hash = LinkHash::of(token).ok_or(ErrorCode::NotFound)?;
            return Ok(Self::Link(hash));
        }
        Authenticated::from_request_parts(parts, state)
            .await
            .map(Self::Actor)
    }
}
