use axum::{
    Json,
    http::{
        HeaderValue,
        header::{RETRY_AFTER, WWW_AUTHENTICATE},
    },
    response::{IntoResponse, Response},
};
use office_documents::DocError;
use office_signup::{Field, SignupError};
use serde::Serialize;
use tracing::error;

use super::ErrorCode;
use crate::request_id;

/// A failed request, rendered as the contract's `Error` body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApiError {
    code: ErrorCode,
    retry_after_secs: Option<u64>,
    latest_version: Option<i32>,
    field: Option<&'static str>,
}

#[derive(Serialize)]
struct ErrorBody<'a> {
    code: ErrorCode,
    message_vi: &'static str,
    request_id: &'a str,
    retryable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    latest_version: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    field: Option<&'static str>,
}

impl ApiError {
    pub fn new(code: ErrorCode) -> Self {
        Self {
            code,
            retry_after_secs: None,
            latest_version: None,
            field: None,
        }
    }

    pub fn rate_limited(retry_after_secs: u64) -> Self {
        Self::retry_after(ErrorCode::RateLimited, retry_after_secs)
    }

    pub fn retry_after(code: ErrorCode, secs: u64) -> Self {
        Self {
            retry_after_secs: Some(secs.max(1)),
            ..Self::new(code)
        }
    }

    /// 409 `VERSION_CONFLICT` naming the version the client must reload.
    pub fn version_conflict(latest_version: i32) -> Self {
        Self {
            latest_version: Some(latest_version),
            ..Self::new(ErrorCode::VersionConflict)
        }
    }

    /// 422 `INVALID_REQUEST` naming the sign-up field that broke a rule.
    pub fn invalid_field(field: &'static str) -> Self {
        Self {
            field: Some(field),
            ..Self::new(ErrorCode::InvalidRequest)
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

impl From<DocError> for ApiError {
    fn from(err: DocError) -> Self {
        let code = match err {
            DocError::VersionConflict { latest_version } => {
                return Self::version_conflict(latest_version);
            }
            DocError::NotFound => ErrorCode::NotFound,
            DocError::CurrentVersion => ErrorCode::VersionIsCurrent,
            DocError::Forbidden => ErrorCode::Forbidden,
            DocError::NameTaken => ErrorCode::NameTaken,
            DocError::FolderCycle => ErrorCode::FolderCycle,
            DocError::QuotaExceeded => ErrorCode::QuotaExceeded,
            DocError::TooLarge => ErrorCode::TooLarge,
            DocError::UnsupportedType => ErrorCode::UnsupportedType,
            DocError::InvalidSignature => ErrorCode::InvalidSignature,
            DocError::InvalidRequest => ErrorCode::InvalidRequest,
            DocError::Unavailable => ErrorCode::Unavailable,
            DocError::Contended => return Self::retry_after(ErrorCode::Unavailable, 1),
            DocError::TooManyPermissions => ErrorCode::TooManyPermissions,
            // The window is a rolling day; an hour frees some of it for a typical sharer.
            DocError::TooManyRecipients => return Self::rate_limited(3600),
        };
        Self::new(code)
    }
}

impl From<SignupError> for ApiError {
    fn from(err: SignupError) -> Self {
        match err {
            SignupError::Closed => ErrorCode::SignupClosed.into(),
            SignupError::Invalid(field) => Self::invalid_field(match field {
                Field::Name => "name",
                Field::Email => "email",
                Field::Password => "password",
            }),
            SignupError::DailyLimit => ErrorCode::SignupDailyLimit.into(),
            SignupError::InvalidToken => ErrorCode::InvalidToken.into(),
            SignupError::Unavailable => ErrorCode::Unavailable.into(),
        }
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
            latest_version: self.latest_version,
            field: self.field,
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
