use std::pin::Pin;

use async_trait::async_trait;
use tokio::io::AsyncRead;

use crate::{ObjectKey, Sha256Digest, StorageError};

/// Bytes streamed into or out of an adapter.
pub type ObjectReader = Pin<Box<dyn AsyncRead + Send>>;

#[derive(Clone, Copy, Debug, Default)]
pub struct PutOptions {
    /// Refuse the write when the content hashes to anything else.
    pub expected_sha256: Option<Sha256Digest>,
    /// Refuse the write when the content is longer.
    pub max_bytes: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PutOutcome {
    pub key: ObjectKey,
    pub sha256: Sha256Digest,
    pub size: u64,
    /// The same content was already stored; nothing new was written.
    pub deduplicated: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObjectMeta {
    pub key: ObjectKey,
    pub size: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Capabilities {
    /// Keys derive from the content, so equal content is stored once.
    pub content_addressed: bool,
    /// `put` returns only after the object survives a crash or power loss.
    pub durable_put: bool,
}

/// One storage backend (filesystem now; NAS or S3 later), spec §11.
#[async_trait]
pub trait StorageAdapter: Send + Sync {
    /// Stores `source` durably. On any error nothing is left behind.
    async fn put(
        &self,
        source: ObjectReader,
        options: PutOptions,
    ) -> Result<PutOutcome, StorageError>;

    async fn get(&self, key: &ObjectKey) -> Result<ObjectReader, StorageError>;

    /// Metadata of `key`, `None` when it does not exist.
    async fn head(&self, key: &ObjectKey) -> Result<Option<ObjectMeta>, StorageError>;

    /// Up to `limit` keys in ascending order, strictly after `after` (orphan sweep).
    async fn list(
        &self,
        after: Option<&ObjectKey>,
        limit: usize,
    ) -> Result<Vec<ObjectKey>, StorageError>;

    /// Removes `key`; `false` when it did not exist.
    async fn delete(&self, key: &ObjectKey) -> Result<bool, StorageError>;

    /// Fails when the backend cannot currently accept writes.
    async fn health(&self) -> Result<(), StorageError>;

    fn capabilities(&self) -> Capabilities;
}
