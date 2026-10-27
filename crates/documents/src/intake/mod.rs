//! Checks on uploaded bytes: signature of the first KB (as the site's intake contract) and, for
//! OOXML, a bounded scan of the ZIP central directory. Both run while the bytes stream to storage.
mod reader;
mod signature;
#[cfg(any(test, feature = "testing"))]
pub mod testing;
mod zip_scan;

pub use reader::ValidatingReader;
pub use signature::{Container, HEAD_BYTES, check_head, server_rules, site_rules};
pub use zip_scan::{ZipLimits, ZipSummary, scan as scan_zip};

/// Why uploaded bytes were refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Rejection {
    #[error("a file variant phase 1 does not store")]
    UnsupportedType,
    #[error("the content does not match its format")]
    InvalidSignature,
    #[error("the archive declares too much content")]
    Bomb,
}
