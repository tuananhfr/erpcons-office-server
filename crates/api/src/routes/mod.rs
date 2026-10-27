pub(crate) mod health;
pub(crate) mod me;

use crate::{ApiError, ErrorCode};

pub(crate) async fn not_found() -> ApiError {
    ErrorCode::NotFound.into()
}
