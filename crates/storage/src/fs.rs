use std::{
    io,
    path::{Path, PathBuf},
};

use async_trait::async_trait;
use sha2::{Digest, Sha256};
use tokio::{
    fs,
    io::{AsyncReadExt, AsyncWriteExt},
};
use uuid::Uuid;

use crate::{
    Capabilities, ObjectKey, ObjectMeta, ObjectReader, PutOptions, PutOutcome, Sha256Digest,
    StorageAdapter, StorageError,
};

const OBJECTS: &str = "objects";
const TMP: &str = "tmp";
const TEMP_SUFFIX: &str = ".part";
const CHUNK: usize = 64 * 1024;

/// Objects at `<root>/objects/<aa>/<bb>/<sha256>`. Writes go through `<root>/tmp`, on the same
/// filesystem, so the final rename is atomic.
pub struct FsStorage {
    root: PathBuf,
}

impl FsStorage {
    /// Opens the store under `root`, creating it if needed, and removes temp files a crash left.
    pub async fn open(root: impl Into<PathBuf>) -> Result<Self, StorageError> {
        let store = Self { root: root.into() };
        fs::create_dir_all(store.objects_dir()).await?;
        fs::create_dir_all(store.tmp_dir()).await?;
        store.remove_stale_temp_files().await?;
        Ok(store)
    }

    fn objects_dir(&self) -> PathBuf {
        self.root.join(OBJECTS)
    }

    fn tmp_dir(&self) -> PathBuf {
        self.root.join(TMP)
    }

    fn temp_path(&self) -> PathBuf {
        self.tmp_dir()
            .join(format!("{}{TEMP_SUFFIX}", Uuid::new_v4()))
    }

    fn object_path(&self, digest: &Sha256Digest) -> PathBuf {
        let hex = digest.to_hex();
        self.objects_dir()
            .join(&hex[0..2])
            .join(&hex[2..4])
            .join(hex)
    }

    /// Only well-formed digests map to a path, so a key can never point outside `objects/`.
    fn path_of(&self, key: &ObjectKey) -> Result<PathBuf, StorageError> {
        let digest: Sha256Digest = key.as_str().parse().map_err(|_| StorageError::InvalidKey)?;
        Ok(self.object_path(&digest))
    }

    async fn remove_stale_temp_files(&self) -> Result<(), StorageError> {
        let mut entries = fs::read_dir(self.tmp_dir()).await?;
        while let Some(entry) = entries.next_entry().await? {
            if entry.file_name().to_string_lossy().ends_with(TEMP_SUFFIX) {
                remove_if_exists(&entry.path()).await?;
            }
        }
        Ok(())
    }

    async fn store(
        &self,
        source: &mut ObjectReader,
        temp: &Path,
        options: PutOptions,
    ) -> Result<PutOutcome, StorageError> {
        let (sha256, size) = write_synced(source, temp, options.max_bytes).await?;
        // Read back what reached the disk, so bytes corrupted on the way never become an object.
        if digest_of_file(temp).await? != sha256 {
            return Err(io::Error::other("read-back digest mismatch").into());
        }
        if options.expected_sha256.is_some_and(|e| e != sha256) {
            return Err(StorageError::DigestMismatch);
        }
        let key = ObjectKey::new(sha256.to_hex());
        let path = self.object_path(&sha256);
        if fs::try_exists(&path).await? {
            return Ok(PutOutcome {
                key,
                sha256,
                size,
                deduplicated: true,
            });
        }
        let shard = path.parent().unwrap_or(&self.root);
        fs::create_dir_all(shard).await?;
        // A freshly created shard directory must be durable too, or the object can vanish with it.
        if let Some(upper) = shard.parent() {
            sync_dir(upper).await?;
        }
        sync_dir(&self.objects_dir()).await?;
        fs::rename(temp, &path).await?;
        sync_dir(shard).await?;
        Ok(PutOutcome {
            key,
            sha256,
            size,
            deduplicated: false,
        })
    }
}

#[async_trait]
impl StorageAdapter for FsStorage {
    async fn put(
        &self,
        mut source: ObjectReader,
        options: PutOptions,
    ) -> Result<PutOutcome, StorageError> {
        let temp = self.temp_path();
        let outcome = self.store(&mut source, &temp, options).await;
        // After a successful rename the temp file is gone and this is a no-op.
        let cleanup = remove_if_exists(&temp).await;
        let outcome = outcome?;
        cleanup?;
        Ok(outcome)
    }

    async fn get(&self, key: &ObjectKey) -> Result<ObjectReader, StorageError> {
        match fs::File::open(self.path_of(key)?).await {
            Ok(file) => Ok(Box::pin(file)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Err(StorageError::NotFound),
            Err(e) => Err(e.into()),
        }
    }

    async fn head(&self, key: &ObjectKey) -> Result<Option<ObjectMeta>, StorageError> {
        match fs::metadata(self.path_of(key)?).await {
            Ok(meta) => Ok(Some(ObjectMeta {
                key: key.clone(),
                size: meta.len(),
            })),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    async fn list(
        &self,
        after: Option<&ObjectKey>,
        limit: usize,
    ) -> Result<Vec<ObjectKey>, StorageError> {
        let after = after.map(ObjectKey::as_str);
        let mut keys = Vec::new();
        if limit == 0 {
            return Ok(keys);
        }
        let objects = self.objects_dir();
        // Shards are the key's first hex pairs, so walking them in order yields keys in order.
        for l1 in sorted_names(&objects).await? {
            if after.is_some_and(|a| l1.as_str() < a.get(..2).unwrap_or(a)) {
                continue;
            }
            for l2 in sorted_names(&objects.join(&l1)).await? {
                for name in sorted_names(&objects.join(&l1).join(&l2)).await? {
                    if name.parse::<Sha256Digest>().is_err()
                        || after.is_some_and(|a| name.as_str() <= a)
                    {
                        continue;
                    }
                    keys.push(ObjectKey::new(name));
                    if keys.len() == limit {
                        return Ok(keys);
                    }
                }
            }
        }
        Ok(keys)
    }

    async fn delete(&self, key: &ObjectKey) -> Result<bool, StorageError> {
        let path = self.path_of(key)?;
        match fs::remove_file(&path).await {
            Ok(()) => {
                if let Some(shard) = path.parent() {
                    sync_dir(shard).await?;
                }
                Ok(true)
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e.into()),
        }
    }

    async fn health(&self) -> Result<(), StorageError> {
        let probe = self.temp_path();
        fs::write(&probe, b"ok").await?;
        fs::remove_file(&probe).await?;
        Ok(())
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            content_addressed: true,
            durable_put: true,
        }
    }
}

/// Streams `source` into a new file at `path`, fsyncs it, and returns its digest and size.
async fn write_synced(
    source: &mut ObjectReader,
    path: &Path,
    max_bytes: Option<u64>,
) -> Result<(Sha256Digest, u64), StorageError> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .await?;
    let mut hasher = Sha256::new();
    let mut size = 0u64;
    let mut buf = vec![0u8; CHUNK];
    loop {
        let n = source.read(&mut buf).await.map_err(StorageError::Source)?;
        if n == 0 {
            break;
        }
        size += n as u64;
        if let Some(max) = max_bytes
            && size > max
        {
            return Err(StorageError::TooLarge(max));
        }
        hasher.update(&buf[..n]);
        file.write_all(&buf[..n]).await?;
    }
    file.flush().await?;
    file.sync_all().await?;
    Ok((Sha256Digest::from_bytes(hasher.finalize().into()), size))
}

async fn digest_of_file(path: &Path) -> io::Result<Sha256Digest> {
    let mut file = fs::File::open(path).await?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; CHUNK];
    loop {
        let n = file.read(&mut buf).await?;
        if n == 0 {
            return Ok(Sha256Digest::from_bytes(hasher.finalize().into()));
        }
        hasher.update(&buf[..n]);
    }
}

async fn sorted_names(dir: &Path) -> io::Result<Vec<String>> {
    let mut names = Vec::new();
    let mut entries = fs::read_dir(dir).await?;
    while let Some(entry) = entries.next_entry().await? {
        if let Ok(name) = entry.file_name().into_string() {
            names.push(name);
        }
    }
    names.sort();
    Ok(names)
}

async fn remove_if_exists(path: &Path) -> io::Result<()> {
    match fs::remove_file(path).await {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

async fn sync_dir(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        fs::File::open(path).await?.sync_all().await
    }
    // Windows cannot open a directory as a file; there the filesystem journals the rename.
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}
