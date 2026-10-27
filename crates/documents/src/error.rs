use office_storage::StorageError;
use tracing::error;

use crate::{
    NotFound, QuotaError,
    intake::{Rejection, ValidatingReader},
    names::InvalidName,
};

/// Why a document service operation failed; each maps to one contract error code.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum DocError {
    #[error("not found")]
    NotFound,
    #[error("name taken")]
    NameTaken,
    #[error("folder cycle")]
    FolderCycle,
    #[error("version conflict")]
    VersionConflict { latest_version: i32 },
    #[error("quota exceeded")]
    QuotaExceeded,
    #[error("file too large")]
    TooLarge,
    #[error("unsupported type")]
    UnsupportedType,
    #[error("invalid signature")]
    InvalidSignature,
    #[error("invalid request")]
    InvalidRequest,
    #[error("unavailable")]
    Unavailable,
}

/// Put into an upload's read error by the HTTP layer when the request body passed its limit.
#[derive(Clone, Copy, Debug, thiserror::Error)]
#[error("request body over its limit")]
pub struct BodyTooLarge;

impl DocError {
    /// The database failed; logged without parameters (they may hold names), answered as 503.
    pub(crate) fn db(err: office_db::Error) -> Self {
        error!(error = %err, "database error");
        Self::Unavailable
    }

    pub(crate) fn storage(err: StorageError) -> Self {
        match err {
            StorageError::TooLarge(_) => Self::TooLarge,
            StorageError::Source(e) => {
                if let Some(rejection) = ValidatingReader::<()>::rejection(&e) {
                    return rejection.into();
                }
                if e.get_ref().is_some_and(|inner| inner.is::<BodyTooLarge>()) {
                    return Self::TooLarge;
                }
                // The client stopped sending (aborted upload, malformed body).
                Self::InvalidRequest
            }
            other => {
                error!(error = %other, "storage error");
                Self::Unavailable
            }
        }
    }
}

impl From<NotFound> for DocError {
    fn from(_: NotFound) -> Self {
        Self::NotFound
    }
}

impl From<InvalidName> for DocError {
    fn from(_: InvalidName) -> Self {
        Self::InvalidRequest
    }
}

impl From<QuotaError> for DocError {
    fn from(e: QuotaError) -> Self {
        match e {
            QuotaError::TooLarge => Self::TooLarge,
            QuotaError::QuotaExceeded => Self::QuotaExceeded,
        }
    }
}

impl From<Rejection> for DocError {
    fn from(r: Rejection) -> Self {
        match r {
            Rejection::UnsupportedType => Self::UnsupportedType,
            // A bomb is refused as content that is not a valid file of its format.
            Rejection::InvalidSignature | Rejection::Bomb => Self::InvalidSignature,
        }
    }
}
