use std::{fmt, sync::Arc};

use axum::{
    extract::Request,
    http::{HeaderName, HeaderValue},
    middleware::Next,
    response::Response,
};
use uuid::Uuid;

pub(crate) const HEADER: HeaderName = HeaderName::from_static("x-request-id");

/// Identifies one request across the response, logs and audit rows (`trace_id`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequestId(Arc<str>);

impl RequestId {
    fn generate() -> Self {
        Self(Uuid::new_v4().to_string().into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RequestId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

tokio::task_local! {
    static CURRENT: RequestId;
}

/// The id of the request being handled, for error bodies built far from the request.
pub(crate) fn current() -> Option<RequestId> {
    CURRENT.try_with(Clone::clone).ok()
}

/// Gives every request a fresh id. A client-sent `X-Request-Id` is ignored, so audit trace ids
/// cannot be forged or made to collide.
pub(crate) async fn assign(mut request: Request, next: Next) -> Response {
    let id = RequestId::generate();
    request.extensions_mut().insert(id.clone());
    let mut response = CURRENT.scope(id.clone(), next.run(request)).await;
    if let Ok(value) = HeaderValue::from_str(id.as_str()) {
        response.headers_mut().insert(HEADER, value);
    }
    response
}
