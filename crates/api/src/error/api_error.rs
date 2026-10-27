use axum::{
    Json,
    http::{
        HeaderValue,
        header::{RETRY_AFTER, WWW_AUTHENTICATE},
    },
    response::{IntoResponse, Response},
};
use serde::Serialize;
use tracing::error;

use super::ErrorCode;
use crate::request_id;

/// A failed request, rendered as the contract's `Error` body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApiError {
    code: ErrorCode,
    retry_after_secs: Option<u64>,
}

#[derive(Serialize)]
struct ErrorBody<'a> {
    code: ErrorCode,
    message_vi: &'static str,
    request_id: &'a str,
    retryable: bool,
}

impl ApiError {
    pub fn new(code: ErrorCode) -> Self {
        Self {
            code,
            retry_after_secs: None,
        }
    }

    pub fn rate_limited(retry_after_secs: u64) -> Self {
        Self {
            code: ErrorCode::RateLimited,
            retry_after_secs: Some(retry_after_secs.max(1)),
        }
    }

    /// 503 for a database failure; the cause is logged, never sent to the client.
    pub fn database(err: office_db::Error) -> Self {
        error!(error = %err, "database error");
        Self::new(ErrorCode::Unavailable)
    }

    pub fn code(&self) -> ErrorCode {
        self.code
    }
}

impl From<ErrorCode> for ApiError {
    fn from(code: ErrorCode) -> Self {
        Self::new(code)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let request_id = request_id::current();
        let body = ErrorBody {
            code: self.code,
            message_vi: self.code.message_vi(),
            request_id: request_id.as_ref().map_or("", |id| id.as_str()),
            retryable: self.code.retryable(),
        };
        let mut response = (self.code.status(), Json(body)).into_response();
        let headers = response.headers_mut();
        if let Some(secs) = self.retry_after_secs {
            headers.insert(RETRY_AFTER, HeaderValue::from(secs));
        }
        if self.code == ErrorCode::Unauthenticated {
            headers.insert(WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
        }
        response
    }
}
