//! Durable writes (plan §4): the bytes are stored first (temp file, fsync, sha256, rename), then
//! one transaction records them; only its commit makes a version exist. Bytes stored by a request
//! that fails later are discarded ([`Documents::discard`]); the orphan sweep removes what a crash
//! leaves. A save by an editor counts against the owner's quota.
use office_db::{
    PgConnection, documents, locks, objects, quota,
    versions::{self, NewVersion},
};
use office_storage::{ObjectKey, ObjectReader, StorageError};
use tracing::error;
use uuid::Uuid;

use super::{
    Caller, Documents, Staged, access,
    audit::{self, Subject},
    folders::active_folder,
};
use crate::{Action, DocError, Document, FailPoint, Format, Name, templates};

pub struct CreateDocument {
    pub folder_id: Option<Uuid>,
    pub title: Option<String>,
    /// Name of the uploaded file; its stem is the title when none is given.
    pub file_name: Option<String>,
    pub staged: Staged,
}

/// The bytes of a document version.
pub struct Content {
    pub reader: ObjectReader,
    pub size: u64,
    pub version: i32,
    pub etag: String,
}

impl Documents {
    /// Creates a document of the caller whose version 1 is `staged`.
    pub async fn create_document(
        &self,
        c: &Caller<'_>,
        request: CreateDocument,
    ) -> Result<Document, DocError> {
        let result = self.create_document_inner(c, request).await;
        self.audit_failure(c, "document.create", Subject::None, &result)
            .await;
        result
    }

    async fn create_document_inner(
        &self,
        c: &Caller<'_>,
        request: CreateDocument,
    ) -> Result<Document, DocError> {
        let staged = &request.staged;
        let title = title_of(&request)?;
        let mut tx = self.begin().await?;
        self.touch_actor(&mut tx, c, c.actor_id()).await?;
        if let Some(folder) = request.folder_id {
            locks::tree(&mut tx, c.actor_id())
                .await
                .map_err(DocError::db)?;
            active_folder(&mut *tx, c, folder, Action::Write).await?;
        }
        let object_id = self.claim_object(&mut tx, c.actor_id(), staged).await?;
        let id = Uuid::now_v7();
        let format = staged.format;
        let draft = documents::NewDocument {
            id,
            owner_id: c.actor_id(),
            folder_id: request.folder_id,
            title: title.as_str(),
            kind: format.kind().as_str(),
            mime: format.mime(),
        };
        documents::insert_draft(&mut *tx, &draft)
            .await
            .map_err(DocError::db)?;
        let sha = staged.sha256.to_hex();
        record_version(&mut tx, c, id, 1, object_id, &sha, size_i64(staged.size)?).await?;
        audit::success(&mut tx, c, "document.create", Subject::Document(id)).await?;
        self.commit(tx).await?;
        access::load(&self.db, c, id, Action::Read)
            .await?
            .document()
    }

    /// Saves a new version based on the one whose etag is in `if_match`. 200 means durable; on any
    /// error no version is recorded.
    pub async fn save_content(
        &self,
        c: &Caller<'_>,
        id: Uuid,
        if_match: &[String],
        source: ObjectReader,
    ) -> Result<Document, DocError> {
        let result = self.save_content_inner(c, id, if_match, source).await;
        self.audit_failure(c, "document.save", Subject::Document(id), &result)
            .await;
        result
    }

    async fn save_content_inner(
        &self,
        c: &Caller<'_>,
        id: Uuid,
        if_match: &[String],
        source: ObjectReader,
    ) -> Result<Document, DocError> {
        // Checked before any byte is stored, so someone who may not write cannot fill the disk.
        let current = access::load_active(&self.db, c, id, Action::Write).await?;
        if !if_match.contains(&current.row.etag) {
            return self.replay_or_conflict(c, current, if_match, source).await;
        }
        let owner_id = current.owner_id().to_owned();
        self.check_room(&owner_id).await?;
        let format = Format::from_mime(&current.row.mime).ok_or(DocError::Unavailable)?;
        let staged = self.stage(format, source).await?;
        let result = self.record_save(c, id, &owner_id, if_match, &staged).await;
        if result.is_err() {
            self.discard(&staged).await;
        }
        result
    }

    async fn record_save(
        &self,
        c: &Caller<'_>,
        id: Uuid,
        owner_id: &str,
        if_match: &[String],
        staged: &Staged,
    ) -> Result<Document, DocError> {
        let mut tx = self.begin().await?;
        self.touch_actor(&mut tx, c, owner_id).await?;
        let object_id = self.claim_object(&mut tx, owner_id, staged).await?;
        // The row lock makes the access check, the etag check and the update below one step: of
        // two saves on the same base the second waits here and then sees the first one's etag, and
        // a permission removed meanwhile refuses this save.
        let target = access::load_for_update(&mut tx, c, id, Action::Write).await?;
        if !if_match.contains(&target.row.etag) {
            return Err(DocError::VersionConflict {
                latest_version: target.row.version,
            });
        }
        let sha = staged.sha256.to_hex();
        let next = target.row.version + 1;
        record_version(
            &mut tx,
            c,
            id,
            next,
            object_id,
            &sha,
            size_i64(staged.size)?,
        )
        .await?;
        audit::success(&mut tx, c, "document.save", Subject::Document(id)).await?;
        self.commit(tx).await?;
        access::load(&self.db, c, id, Action::Read)
            .await?
            .document()
    }

    pub async fn open_content(&self, c: &Caller<'_>, id: Uuid) -> Result<Content, DocError> {
        access::load(&self.db, c, id, Action::Read).await?;
        self.current_content(id).await
    }

    /// The current bytes of `id`, which the caller was allowed to read.
    pub(super) async fn current_content(&self, id: Uuid) -> Result<Content, DocError> {
        let content = documents::content_ref(&self.db, id)
            .await
            .map_err(DocError::db)?
            .ok_or(DocError::NotFound)?;
        Ok(Content {
            reader: self.open_object(id, content.opaque_key).await?,
            size: u64::try_from(content.size).unwrap_or(0),
            version: content.version,
            etag: content.etag,
        })
    }

    pub(super) async fn open_object(
        &self,
        id: Uuid,
        opaque_key: String,
    ) -> Result<ObjectReader, DocError> {
        match self.storage.get(&ObjectKey::new(opaque_key)).await {
            Ok(reader) => Ok(reader),
            Err(StorageError::NotFound) => {
                error!(document_id = %id, "bytes of a version are missing");
                Err(DocError::Unavailable)
            }
            Err(e) => Err(DocError::storage(e)),
        }
    }

    /// Takes the object's lock, checks the bytes are still there (a sweep may have removed them
    /// between `put` and now) and that the owner's quota allows them, then records the object.
    async fn claim_object(
        &self,
        conn: &mut PgConnection,
        owner_id: &str,
        staged: &Staged,
    ) -> Result<Uuid, DocError> {
        let sha = staged.sha256.to_hex();
        locks::object(conn, &sha).await.map_err(DocError::db)?;
        self.require_stored(&staged.key).await?;
        let used = quota::used_bytes(&mut *conn, owner_id)
            .await
            .map_err(DocError::db)?;
        let counted = quota::already_counted(&mut *conn, owner_id, &sha)
            .await
            .map_err(DocError::db)?;
        let incoming = if counted { 0 } else { staged.size };
        self.quota
            .check(u64::try_from(used).unwrap_or(0), incoming)?;
        objects::upsert(conn, staged.key.as_str(), &sha, size_i64(staged.size)?)
            .await
            .map_err(DocError::db)
    }

    /// The bytes behind `key` still exist; call with the object's lock held.
    pub(super) async fn require_stored(&self, key: &ObjectKey) -> Result<(), DocError> {
        match self.storage.head(key).await {
            Ok(Some(_)) => Ok(()),
            Ok(None) => Err(DocError::Unavailable),
            Err(e) => Err(DocError::storage(e)),
        }
    }

    pub(super) async fn commit(
        &self,
        tx: office_db::Transaction<'static, office_db::Postgres>,
    ) -> Result<(), DocError> {
        self.fail_points.pause(FailPoint::BeforeCommit).await;
        if self.fail_points.fires(FailPoint::BeforeCommit) {
            // Dropping the transaction rolls it back, as a lost connection would.
            return Err(DocError::Unavailable);
        }
        tx.commit().await.map_err(DocError::db)
    }
}

/// Inserts version `seq`, written by the caller, and makes it current with a fresh etag.
pub(super) async fn record_version(
    conn: &mut PgConnection,
    c: &Caller<'_>,
    document_id: Uuid,
    seq: i32,
    object_id: Uuid,
    sha256: &str,
    bytes: i64,
) -> Result<(), DocError> {
    let version_id = Uuid::now_v7();
    let version = NewVersion {
        id: version_id,
        document_id,
        seq,
        storage_object_id: object_id,
        sha256,
        bytes,
        author_id: c.actor_id(),
        author_name: &c.actor.display_name,
    };
    versions::insert(&mut *conn, &version)
        .await
        .map_err(DocError::db)?;
    documents::set_current_version(conn, document_id, version_id, &version_etag(version_id))
        .await
        .map_err(DocError::db)
}

/// A document's etag names its current version, so the etag of any version is known.
pub(super) fn version_etag(version_id: Uuid) -> String {
    format!("\"{}\"", version_id.simple())
}

fn size_i64(size: u64) -> Result<i64, DocError> {
    i64::try_from(size).map_err(|_| DocError::TooLarge)
}

/// The given title, else the uploaded file's name without its extension, else a default.
fn title_of(request: &CreateDocument) -> Result<Name, DocError> {
    if let Some(title) = &request.title {
        return Ok(Name::parse(title)?);
    }
    let from_file = request
        .file_name
        .as_deref()
        .map(|n| n.rsplit_once('.').map_or(n, |(stem, _)| stem))
        .and_then(|stem| Name::parse(stem).ok());
    match from_file {
        Some(name) => Ok(name),
        None => Ok(Name::parse(templates::default_title(
            request.staged.format,
        ))?),
    }
}
