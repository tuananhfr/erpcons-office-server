//! axum's extractors with their rejections turned into the contract's `Error` body.
use axum::{
    Json,
    body::Bytes,
    extract::{FromRequest, FromRequestParts, Path, Query, Request},
    http::{HeaderMap, StatusCode, header::IF_MATCH, request::Parts},
};
use serde::de::DeserializeOwned;

use crate::{ApiError, ErrorCode};

/// 413 stays 413 (body over the limit, chunked or not); every other parse failure is 422.
fn rejected(status: StatusCode) -> ApiError {
    if status == StatusCode::PAYLOAD_TOO_LARGE {
        ErrorCode::TooLarge.into()
    } else {
        ErrorCode::InvalidRequest.into()
    }
}

pub(crate) struct ApiJson<T>(pub T);

impl<T: DeserializeOwned, S: Send + Sync> FromRequest<S> for ApiJson<T> {
    type Rejection = ApiError;

    async fn from_request(req: Request, state: &S) -> Result<Self, ApiError> {
        Json::<T>::from_request(req, state)
            .await
            .map(|Json(v)| Self(v))
            .map_err(|r| rejected(r.status()))
    }
}

/// A JSON body that may be left out: no body at all is `T::default()`.
pub(crate) struct OptionalJson<T>(pub T);

impl<T: DeserializeOwned + Default, S: Send + Sync> FromRequest<S> for OptionalJson<T> {
    type Rejection = ApiError;

    async fn from_request(req: Request, state: &S) -> Result<Self, ApiError> {
        let bytes = Bytes::from_request(req, state)
            .await
            .map_err(|r| rejected(r.status()))?;
        if bytes.is_empty() {
            return Ok(Self(T::default()));
        }
        serde_json::from_slice(&bytes)
            .map(Self)
            .map_err(|_| ErrorCode::InvalidRequest.into())
    }
}

pub(crate) struct ApiPath<T>(pub T);

impl<T: DeserializeOwned + Send, S: Send + Sync> FromRequestParts<S> for ApiPath<T> {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, ApiError> {
        Path::<T>::from_request_parts(parts, state)
            .await
            .map(|Path(v)| Self(v))
            .map_err(|r| rejected(r.status()))
    }
}

pub(crate) struct ApiQuery<T>(pub T);

impl<T: DeserializeOwned, S: Send + Sync> FromRequestParts<S> for ApiQuery<T> {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, ApiError> {
        Query::<T>::from_request_parts(parts, state)
            .await
            .map(|Query(v)| Self(v))
            .map_err(|r| rejected(r.status()))
    }
}

/// `Idempotency-Key`, required on the create operations: one header of 1–100 characters.
pub(crate) struct IdempotencyKey(pub String);

impl<S: Send + Sync> FromRequestParts<S> for IdempotencyKey {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, ApiError> {
        let mut values = parts.headers.get_all("idempotency-key").iter();
        let key = match (values.next(), values.next()) {
            (Some(v), None) => v.to_str().ok(),
            _ => None,
        };
        key.filter(|k| (1..=100).contains(&k.chars().count()))
            .map(|k| Self(k.to_owned()))
            .ok_or_else(|| ErrorCode::InvalidRequest.into())
    }
}

/// The strong entity tags of `If-Match`; `None` when the header is absent. `*` is refused: a save
/// must name the version it is based on. Weak tags never match strongly and are dropped.
pub(crate) fn if_match(headers: &HeaderMap) -> Result<Option<Vec<String>>, ApiError> {
    let mut tags = Vec::new();
    let mut present = false;
    for value in headers.get_all(IF_MATCH) {
        present = true;
        let value = value.to_str().map_err(|_| ErrorCode::InvalidRequest)?;
        for tag in value.split(',').map(str::trim).filter(|t| !t.is_empty()) {
            if tag == "*" {
                return Err(ErrorCode::InvalidRequest.into());
            }
            if !tag.starts_with("W/") {
                tags.push(tag.to_owned());
            }
        }
    }
    if !present {
        return Ok(None);
    }
    if tags.is_empty() {
        return Err(ErrorCode::InvalidRequest.into());
    }
    Ok(Some(tags))
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderValue;

    use super::*;

    fn headers(values: &[&str]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for v in values {
            map.append(IF_MATCH, HeaderValue::from_str(v).expect("header"));
        }
        map
    }

    #[test]
    fn if_match_lists_strong_tags() {
        assert_eq!(if_match(&headers(&[])), Ok(None));
        assert_eq!(
            if_match(&headers(&["\"a\", W/\"b\"", "\"c\""])),
            Ok(Some(vec!["\"a\"".into(), "\"c\"".into()]))
        );
        assert!(if_match(&headers(&["*"])).is_err());
        assert!(if_match(&headers(&["W/\"weak\""])).is_err());
        assert!(if_match(&headers(&[""])).is_err());
    }
}
