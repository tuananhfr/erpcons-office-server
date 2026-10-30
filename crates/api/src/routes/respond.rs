use axum::{
    Json,
    body::Body,
    http::{
        HeaderName, HeaderValue, StatusCode,
        header::{CONTENT_LENGTH, CONTENT_TYPE, ETAG, X_CONTENT_TYPE_OPTIONS},
    },
    response::{IntoResponse, Response},
};
use office_documents::{Caller, Content, Document};
use office_identity::Actor;
use tokio_util::io::ReaderStream;

use crate::{RequestId, dto::DocumentDto, idempotency::Stored};

pub(crate) fn caller<'a>(actor: &'a Actor, request_id: &'a RequestId) -> Caller<'a> {
    Caller {
        actor,
        trace_id: request_id.as_str(),
    }
}

/// A `Document` body with the current version's `ETag`.
pub(crate) fn document(status: StatusCode, doc: &Document) -> Response {
    let mut response = (status, Json(DocumentDto::from(doc))).into_response();
    if let Ok(etag) = HeaderValue::from_str(&doc.etag) {
        response.headers_mut().insert(ETAG, etag);
    }
    response
}

/// The same, kept for idempotent replays.
pub(crate) fn stored_document(status: StatusCode, doc: &Document) -> Stored {
    Stored::new(status, Some(&doc.etag), DocumentDto::from(doc))
}

const X_DOCUMENT_VERSION: HeaderName = HeaderName::from_static("x-document-version");

/// Streams the bytes of a version; never held in memory whole.
pub(crate) fn content(content: Content) -> Response {
    let mut response = Response::new(Body::from_stream(ReaderStream::new(content.reader)));
    let headers = response.headers_mut();
    headers.insert(
        CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    headers.insert(X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    headers.insert(CONTENT_LENGTH, HeaderValue::from(content.size));
    headers.insert(X_DOCUMENT_VERSION, HeaderValue::from(content.version));
    if !content.etag.is_empty()
        && let Ok(etag) = HeaderValue::from_str(&content.etag)
    {
        headers.insert(ETAG, etag);
    }
    response
}
