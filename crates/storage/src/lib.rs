//! Byte storage of the document service: the `StorageAdapter` trait and `FsStorage`, a filesystem
//! adapter that stores each object once under its sha256.
mod adapter;
mod digest;
mod error;
mod fs;
mod hash_stream;

pub use adapter::{Capabilities, ObjectMeta, ObjectReader, PutOptions, PutOutcome, StorageAdapter};
pub use digest::{ObjectKey, Sha256Digest};
pub use error::StorageError;
pub use fs::FsStorage;
pub use hash_stream::digest_stream;
