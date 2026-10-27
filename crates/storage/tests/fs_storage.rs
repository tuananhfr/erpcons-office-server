use std::{
    io::{self, Cursor},
    path::Path,
    pin::Pin,
    task::{Context, Poll},
};

use office_storage::{
    FsStorage, ObjectKey, ObjectReader, PutOptions, Sha256Digest, StorageAdapter, StorageError,
};
use tokio::io::{AsyncRead, AsyncReadExt, ReadBuf};

fn source(bytes: &[u8]) -> ObjectReader {
    Box::pin(Cursor::new(bytes.to_vec()))
}

/// Yields one chunk, then fails like an aborted upload.
struct FailsAfterFirstChunk {
    sent: bool,
}

impl AsyncRead for FailsAfterFirstChunk {
    fn poll_read(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if this.sent {
            return Poll::Ready(Err(io::Error::other("connection reset")));
        }
        this.sent = true;
        let n = buf.remaining().min(10_000);
        buf.put_slice(&vec![7u8; n]);
        Poll::Ready(Ok(()))
    }
}

async fn read_all(store: &FsStorage, key: &ObjectKey) -> Vec<u8> {
    let mut out = Vec::new();
    store
        .get(key)
        .await
        .expect("get")
        .read_to_end(&mut out)
        .await
        .expect("read");
    out
}

fn files_under(dir: &Path) -> Vec<String> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(dir).expect("read_dir") {
        let path = entry.expect("entry").path();
        if path.is_dir() {
            found.extend(files_under(&path));
        } else {
            found.push(path.display().to_string());
        }
    }
    found
}

async fn assert_nothing_stored(root: &Path, store: &FsStorage) {
    assert!(
        files_under(&root.join("objects")).is_empty(),
        "no object may be left"
    );
    assert!(
        files_under(&root.join("tmp")).is_empty(),
        "no temp file may be left"
    );
    assert!(store.list(None, 10).await.expect("list").is_empty());
}

#[tokio::test]
async fn put_stores_content_under_its_sha256() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = FsStorage::open(dir.path()).await.expect("open");
    let out = store
        .put(source(b"hello"), PutOptions::default())
        .await
        .expect("put");
    let digest = Sha256Digest::of(b"hello");
    assert_eq!(out.sha256, digest);
    assert_eq!(out.key.as_str(), digest.to_hex());
    assert_eq!(out.size, 5);
    assert!(!out.deduplicated);
    let hex = digest.to_hex();
    assert!(
        dir.path()
            .join("objects")
            .join(&hex[..2])
            .join(&hex[2..4])
            .join(&hex)
            .is_file()
    );
    assert_eq!(read_all(&store, &out.key).await, b"hello");
    let meta = store.head(&out.key).await.expect("head").expect("exists");
    assert_eq!(meta.size, 5);
    assert!(files_under(&dir.path().join("tmp")).is_empty());
}

#[tokio::test]
async fn equal_content_is_stored_once() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = FsStorage::open(dir.path()).await.expect("open");
    let first = store
        .put(source(b"same"), PutOptions::default())
        .await
        .expect("put");
    let second = store
        .put(source(b"same"), PutOptions::default())
        .await
        .expect("put");
    assert_eq!(first.key, second.key);
    assert!(second.deduplicated);
    assert_eq!(files_under(&dir.path().join("objects")).len(), 1);
    assert!(files_under(&dir.path().join("tmp")).is_empty());
}

#[tokio::test]
async fn failing_source_leaves_no_partial_object() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = FsStorage::open(dir.path()).await.expect("open");
    let err = store
        .put(
            Box::pin(FailsAfterFirstChunk { sent: false }),
            PutOptions::default(),
        )
        .await
        .expect_err("must fail");
    assert!(matches!(err, StorageError::Source(_)), "{err:?}");
    assert_nothing_stored(dir.path(), &store).await;
}

#[tokio::test]
async fn digest_mismatch_leaves_no_object() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = FsStorage::open(dir.path()).await.expect("open");
    let options = PutOptions {
        expected_sha256: Some(Sha256Digest::of(b"other")),
        ..PutOptions::default()
    };
    let err = store
        .put(source(b"content"), options)
        .await
        .expect_err("must fail");
    assert!(matches!(err, StorageError::DigestMismatch), "{err:?}");
    assert_nothing_stored(dir.path(), &store).await;
}

#[tokio::test]
async fn oversized_content_leaves_no_object() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = FsStorage::open(dir.path()).await.expect("open");
    let options = PutOptions {
        max_bytes: Some(4),
        ..PutOptions::default()
    };
    let err = store
        .put(source(b"12345"), options)
        .await
        .expect_err("must fail");
    assert!(matches!(err, StorageError::TooLarge(4)), "{err:?}");
    assert_nothing_stored(dir.path(), &store).await;
    let fits = PutOptions {
        max_bytes: Some(5),
        ..PutOptions::default()
    };
    store.put(source(b"12345"), fits).await.expect("fits");
}

#[tokio::test]
async fn expected_digest_that_matches_is_accepted() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = FsStorage::open(dir.path()).await.expect("open");
    let options = PutOptions {
        expected_sha256: Some(Sha256Digest::of(b"content")),
        ..PutOptions::default()
    };
    store.put(source(b"content"), options).await.expect("put");
}

#[tokio::test]
async fn open_removes_temp_files_left_by_a_crash() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(dir.path().join("tmp")).expect("mkdir");
    std::fs::write(dir.path().join("tmp").join("crashed.part"), b"half").expect("write");
    let store = FsStorage::open(dir.path()).await.expect("open");
    assert_nothing_stored(dir.path(), &store).await;
}

#[tokio::test]
async fn missing_and_invalid_keys() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = FsStorage::open(dir.path()).await.expect("open");
    let missing = ObjectKey::new(Sha256Digest::of(b"absent").to_hex());
    assert!(matches!(
        store.get(&missing).await,
        Err(StorageError::NotFound)
    ));
    assert_eq!(store.head(&missing).await.expect("head"), None);
    assert!(!store.delete(&missing).await.expect("delete"));
    let traversal = ObjectKey::new("../../etc/passwd");
    assert!(matches!(
        store.get(&traversal).await,
        Err(StorageError::InvalidKey)
    ));
    assert!(matches!(
        store.delete(&traversal).await,
        Err(StorageError::InvalidKey)
    ));
}

#[tokio::test]
async fn delete_removes_the_object() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = FsStorage::open(dir.path()).await.expect("open");
    let out = store
        .put(source(b"bye"), PutOptions::default())
        .await
        .expect("put");
    assert!(store.delete(&out.key).await.expect("delete"));
    assert_eq!(store.head(&out.key).await.expect("head"), None);
}

#[tokio::test]
async fn list_pages_keys_in_order() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = FsStorage::open(dir.path()).await.expect("open");
    let mut expected = Vec::new();
    for i in 0..7u8 {
        let out = store
            .put(source(&[i; 3]), PutOptions::default())
            .await
            .expect("put");
        expected.push(out.key);
    }
    expected.sort();
    let first = store.list(None, 3).await.expect("list");
    assert_eq!(first, expected[..3]);
    let rest = store.list(first.last(), 10).await.expect("list");
    assert_eq!(rest, expected[3..]);
    assert!(store.list(None, 0).await.expect("list").is_empty());
}

#[tokio::test]
async fn health_and_capabilities() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("store");
    let store = FsStorage::open(&root).await.expect("open");
    store.health().await.expect("healthy");
    let caps = store.capabilities();
    assert!(caps.content_addressed && caps.durable_put);
    std::fs::remove_dir_all(&root).expect("remove");
    assert!(store.health().await.is_err());
}

#[tokio::test]
async fn storing_equal_content_again_refreshes_its_modification_time() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = FsStorage::open(dir.path()).await.expect("open");
    let out = store
        .put(source(b"aged"), PutOptions::default())
        .await
        .expect("put");
    let hex = out.key.as_str().to_owned();
    let path = dir
        .path()
        .join("objects")
        .join(&hex[..2])
        .join(&hex[2..4])
        .join(&hex);
    let two_hours_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(7200);
    std::fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .expect("open object")
        .set_modified(two_hours_ago)
        .expect("age it");
    let aged = store.head(&out.key).await.expect("head").expect("exists");
    assert!(aged.modified.expect("mtime") <= two_hours_ago);
    let again = store
        .put(source(b"aged"), PutOptions::default())
        .await
        .expect("put");
    assert!(again.deduplicated);
    let fresh = store.head(&out.key).await.expect("head").expect("exists");
    assert!(fresh.modified.expect("mtime") > two_hours_ago);
}
