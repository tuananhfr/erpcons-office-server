use axum::{
    Json,
    http::{HeaderValue, StatusCode, header::ETAG},
    response::{IntoResponse, Response},
};
use office_documents::{Caller, Document};
use office_identity::Actor;

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
