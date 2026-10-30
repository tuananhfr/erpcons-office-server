//! Opening a public view link, without signing in. The token comes in the `X-Link-Token` header,
//! never the URL, so no access log, proxy or `Referer` carries it; the site keeps it in the page's
//! fragment. The service audits the document id, never the token.
use std::time::Instant;

use axum::{
    Extension, Json,
    extract::State,
    http::{HeaderMap, HeaderValue, header::REFERRER_POLICY},
    response::{IntoResponse, Response},
};

use super::respond;
use crate::{ApiError, AppState, ErrorCode, RequestId, dto::DocumentDto, rate_limit::ClientKey};

const TOKEN_HEADER: &str = "x-link-token";

/// A missing or unreadable header answers as an unknown token does.
fn token(headers: &HeaderMap) -> Result<&str, ApiError> {
    headers
        .get(TOKEN_HEADER)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| ErrorCode::NotFound.into())
}

/// Guessing a 256-bit token is hopeless; the limit is against a flood of downloads per address.
fn check_rate(state: &AppState, client: Option<Extension<ClientKey>>) -> Result<(), ApiError> {
    let key = client.map_or(ClientKey(None), |Extension(k)| k);
    state
        .link_limiter
        .check(key, Instant::now())
        .map_err(ApiError::rate_limited)
}

fn no_referrer(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    response
}

pub(crate) async fn open(
    State(state): State<AppState>,
    Extension(rid): Extension<RequestId>,
    client: Option<Extension<ClientKey>>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    check_rate(&state, client)?;
    let doc = state.docs.open_link(token(&headers)?, rid.as_str()).await?;
    Ok(no_referrer(Json(DocumentDto::from(&doc)).into_response()))
}

pub(crate) async fn content(
    State(state): State<AppState>,
    Extension(rid): Extension<RequestId>,
    client: Option<Extension<ClientKey>>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    check_rate(&state, client)?;
    let content = state
        .docs
        .open_link_content(token(&headers)?, rid.as_str())
        .await?;
    Ok(no_referrer(respond::content(content)))
}
