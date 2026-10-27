pub(crate) mod content;
pub(crate) mod documents;
pub(crate) mod folders;
pub(crate) mod health;
pub(crate) mod me;
mod respond;
pub(crate) mod trash;
pub(crate) mod upload;

use crate::{ApiError, ErrorCode};

pub(crate) async fn not_found() -> ApiError {
    ErrorCode::NotFound.into()
}

pub(crate) async fn method_not_allowed() -> ApiError {
    ErrorCode::MethodNotAllowed.into()
}
