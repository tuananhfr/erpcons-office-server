pub(crate) mod content;
pub(crate) mod documents;
pub(crate) mod folders;
pub(crate) mod health;
pub(crate) mod link;
pub(crate) mod me;
pub(crate) mod permissions;
pub(crate) mod public_links;
mod respond;
pub(crate) mod share_settings;
pub(crate) mod shared;
pub(crate) mod signup;
pub(crate) mod trash;
pub(crate) mod upload;
pub(crate) mod versions;

use crate::{ApiError, ErrorCode};

pub(crate) async fn not_found() -> ApiError {
    ErrorCode::NotFound.into()
}

pub(crate) async fn method_not_allowed() -> ApiError {
    ErrorCode::MethodNotAllowed.into()
}
