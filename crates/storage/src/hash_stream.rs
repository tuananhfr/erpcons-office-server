use sha2::{Digest, Sha256};
use tokio::io::AsyncReadExt;

use crate::{ObjectReader, Sha256Digest, StorageError};

const CHUNK: usize = 64 * 1024;

/// The sha256 of `source` without storing it; `TooLarge` once it passes `max_bytes`.
pub async fn digest_stream(
    mut source: ObjectReader,
    max_bytes: u64,
) -> Result<Sha256Digest, StorageError> {
    let mut hasher = Sha256::new();
    let mut size = 0u64;
    let mut buf = vec![0u8; CHUNK];
    loop {
        let n = source.read(&mut buf).await.map_err(StorageError::Source)?;
        if n == 0 {
            return Ok(Sha256Digest::from_bytes(hasher.finalize().into()));
        }
        size += n as u64;
        if size > max_bytes {
            return Err(StorageError::TooLarge(max_bytes));
        }
        hasher.update(&buf[..n]);
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    #[tokio::test]
    async fn hashes_within_the_limit_and_refuses_beyond_it() {
        let bytes = b"abc".to_vec();
        let digest = digest_stream(Box::pin(Cursor::new(bytes.clone())), 3)
            .await
            .expect("digest");
        assert_eq!(digest, Sha256Digest::of(&bytes));
        let over = digest_stream(Box::pin(Cursor::new(bytes)), 2).await;
        assert!(matches!(over, Err(StorageError::TooLarge(2))));
    }
}
