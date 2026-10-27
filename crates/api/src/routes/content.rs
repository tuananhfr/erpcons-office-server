use std::io;

use axum::{
    Extension,
    body::Body,
    extract::State,
    http::{
        HeaderMap, HeaderName, HeaderValue, StatusCode,
        header::{CONTENT_LENGTH, CONTENT_TYPE, ETAG, X_CONTENT_TYPE_OPTIONS},
    },
    response::Response,
};
use futures_util::TryStreamExt;
use tokio_util::io::{ReaderStream, StreamReader};
use uuid::Uuid;

use super::respond::{self, caller};
use crate::{
    ApiError, AppState, Authenticated, ErrorCode, RequestId,
    extract::{ApiPath, if_match},
};

const X_DOCUMENT_VERSION: HeaderName = HeaderName::from_static("x-document-version");

/// Streams the current version's bytes.
pub(crate) async fn get(
    State(state): State<AppState>,
    Extension(rid): Extension<RequestId>,
    Authenticated(actor): Authenticated,
    ApiPath(id): ApiPath<Uuid>,
) -> Result<Response, ApiError> {
    let content = state.docs.open_content(&caller(&actor, &rid), id).await?;
    let mut response = Response::new(Body::from_stream(ReaderStream::new(content.reader)));
    let headers = response.headers_mut();
    headers.insert(
        CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    headers.insert(X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    headers.insert(CONTENT_LENGTH, HeaderValue::from(content.size));
    headers.insert(X_DOCUMENT_VERSION, HeaderValue::from(content.version));
    if let Ok(etag) = HeaderValue::from_str(&content.etag) {
        headers.insert(ETAG, etag);
    }
    Ok(response)
}

/// Saves a new version; the body streams to storage, it is never held in memory whole.
pub(crate) async fn put(
    State(state): State<AppState>,
    Extension(rid): Extension<RequestId>,
    Authenticated(actor): Authenticated,
    ApiPath(id): ApiPath<Uuid>,
    headers: HeaderMap,
    body: Body,
) -> Result<Response, ApiError> {
    let tags = if_match(&headers)?.ok_or(ErrorCode::PreconditionRequired)?;
    let reader = StreamReader::new(body.into_data_stream().map_err(io::Error::other));
    let doc = state
        .docs
        .save_content(&caller(&actor, &rid), id, &tags, Box::pin(reader))
        .await?;
    Ok(respond::document(StatusCode::OK, &doc))
}
