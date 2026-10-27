use std::io;

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("object not found")]
    NotFound,
    #[error("invalid object key")]
    InvalidKey,
    #[error("content does not match the expected sha256")]
    DigestMismatch,
    #[error("object is larger than {0} bytes")]
    TooLarge(u64),
    /// Reading the caller's source failed (for example an aborted upload); storage itself is fine.
    #[error("reading the source failed: {0}")]
    Source(io::Error),
    #[error("storage I/O failed: {0}")]
    Io(#[from] io::Error),
}
