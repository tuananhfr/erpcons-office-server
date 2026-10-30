//! Old versions: reading their bytes, restoring one as a new version that points at the same
//! stored object (no bytes copied, no quota charged: the object is already counted), and deleting
//! one, which frees its bytes only when no other version uses them.
use office_db::{locks, purge, versions};
use office_storage::ObjectKey;
use tracing::error;
use uuid::Uuid;

use super::{
    Caller, Content, Documents, access,
    audit::{self, Subject},
    content::record_version,
};
use crate::{Action, DocError, Document, FailPoint};

impl Documents {
    /// The bytes of version `seq`, the current one included.
    pub async fn version_content(
        &self,
        c: &Caller<'_>,
        id: Uuid,
        seq: i32,
    ) -> Result<Content, DocError> {
        access::load(&self.db, c, id, Action::ReadVersions).await?;
        let version = versions::content_of(&self.db, id, seq)
            .await
            .map_err(DocError::db)?
            .ok_or(DocError::NotFound)?;
        Ok(Content {
            reader: self.open_object(id, version.opaque_key).await?,
            size: u64::try_from(version.bytes).unwrap_or(0),
            version: seq,
            etag: String::new(),
        })
    }

    /// Makes version `seq` current again as version `latest + 1`, if `if_match` names the current
    /// version.
    pub async fn restore_version(
        &self,
        c: &Caller<'_>,
        id: Uuid,
        seq: i32,
        if_match: &[String],
    ) -> Result<Document, DocError> {
        let result = self.restore_version_inner(c, id, seq, if_match).await;
        self.audit_failure(
            c,
            "document.version_restore",
            Subject::Document(id),
            &result,
        )
        .await;
        result
    }

    /// Deletes old version `seq` for good (owner only). Its number is never reused. The bytes are
    /// deleted, and leave the owner's quota, only when no other version still uses them.
    pub async fn delete_version(&self, c: &Caller<'_>, id: Uuid, seq: i32) -> Result<(), DocError> {
        let result = self.delete_version_inner(c, id, seq).await;
        self.audit_failure(c, "document.version_delete", Subject::Document(id), &result)
            .await;
        result
    }

    async fn delete_version_inner(
        &self,
        c: &Caller<'_>,
        id: Uuid,
        seq: i32,
    ) -> Result<(), DocError> {
        let current = access::load_active(&self.db, c, id, Action::DeleteVersion).await?;
        let old = versions::content_of(&self.db, id, seq)
            .await
            .map_err(DocError::db)?
            .ok_or(DocError::NotFound)?;
        let mut tx = self.begin().await?;
        self.touch_actor(&mut tx, c, current.owner_id()).await?;
        // A save of the same content holds this lock until its commit, so whether the object is
        // still used is decided after that save, never during it.
        locks::object(&mut tx, &old.sha256)
            .await
            .map_err(DocError::db)?;
        let target = access::load_for_update(&mut tx, c, id, Action::DeleteVersion).await?;
        if seq == target.row.version {
            return Err(DocError::CurrentVersion);
        }
        if !versions::delete(&mut *tx, id, seq)
            .await
            .map_err(DocError::db)?
        {
            return Err(DocError::NotFound);
        }
        let unused = purge::release_if_unused(&mut tx, old.storage_object_id)
            .await
            .map_err(DocError::db)?;
        audit::success(&mut tx, c, "document.version_delete", Subject::Document(id)).await?;
        tx.commit().await.map_err(DocError::db)?;
        if unused && !self.fail_points.fires(FailPoint::AfterVersionDelete) {
            let object = purge::DeadObject {
                id: old.storage_object_id,
                opaque_key: old.opaque_key,
                sha256: old.sha256,
            };
            // Already committed: the purge job deletes what is left `DELETE_PENDING` here.
            if let Err(e) = self.delete_dead_object(&object).await {
                error!(error = %e, "deleting the bytes of a deleted version failed");
            }
        }
        Ok(())
    }

    async fn restore_version_inner(
        &self,
        c: &Caller<'_>,
        id: Uuid,
        seq: i32,
        if_match: &[String],
    ) -> Result<Document, DocError> {
        let current = access::load_active(&self.db, c, id, Action::Write).await?;
        let old = versions::content_of(&self.db, id, seq)
            .await
            .map_err(DocError::db)?
            .ok_or(DocError::NotFound)?;
        let mut tx = self.begin().await?;
        self.touch_actor(&mut tx, c, current.owner_id()).await?;
        // The object's lock keeps the purge job from deleting its bytes until the commit.
        locks::object(&mut tx, &old.sha256)
            .await
            .map_err(DocError::db)?;
        self.require_stored(&ObjectKey::new(old.opaque_key.clone()))
            .await?;
        let target = access::load_for_update(&mut tx, c, id, Action::Write).await?;
        if !if_match.contains(&target.row.etag) {
            return Err(DocError::VersionConflict {
                latest_version: target.row.version,
            });
        }
        let next = target.row.version + 1;
        record_version(
            &mut tx,
            c,
            id,
            next,
            old.storage_object_id,
            &old.sha256,
            old.bytes,
        )
        .await?;
        audit::success(
            &mut tx,
            c,
            "document.version_restore",
            Subject::Document(id),
        )
        .await?;
        self.commit(tx).await?;
        access::load(&self.db, c, id, Action::Read)
            .await?
            .document()
    }
}
