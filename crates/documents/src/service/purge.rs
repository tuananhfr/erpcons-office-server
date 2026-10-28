//! Purge on the owner's request: an item in the trash goes for good, as the purge job does it after
//! the retention period (metadata kept as `PURGED`, bytes nothing live shares deleted).
use office_db::{ItemState, locks, purge};
use tracing::error;
use uuid::Uuid;

use super::{
    Caller, Documents,
    audit::{self, Subject},
    documents::owned_document,
    folders::owned_folder,
};
use crate::{Action, DocError};

impl Documents {
    /// Purges a trashed document; anything else, active ones included, is [`DocError::NotFound`].
    pub async fn purge_document(&self, c: &Caller<'_>, id: Uuid) -> Result<(), DocError> {
        let result = self.purge_document_inner(c, id).await;
        self.audit_failure(c, "document.purge", Subject::Document(id), &result)
            .await;
        result
    }

    async fn purge_document_inner(&self, c: &Caller<'_>, id: Uuid) -> Result<(), DocError> {
        let mut tx = self.begin().await?;
        locks::tree(&mut tx, c.owner())
            .await
            .map_err(DocError::db)?;
        let row = owned_document(&mut *tx, c, id, Action::Purge).await?;
        if row.state != ItemState::Trashed {
            return Err(DocError::NotFound);
        }
        let marked = purge::mark_pending(&mut *tx, c.owner(), &[id])
            .await
            .map_err(DocError::db)?;
        if marked.is_empty() {
            return Err(DocError::NotFound);
        }
        audit::success(&mut tx, c, "document.purge", Subject::Document(id)).await?;
        tx.commit().await.map_err(DocError::db)?;
        self.release_purged(&marked).await;
        Ok(())
    }

    /// Purges a trashed folder with what was trashed together with it.
    pub async fn purge_folder(&self, c: &Caller<'_>, id: Uuid) -> Result<(), DocError> {
        let result = self.purge_folder_inner(c, id).await;
        self.audit_failure(c, "folder.purge", Subject::Folder(id), &result)
            .await;
        result
    }

    async fn purge_folder_inner(&self, c: &Caller<'_>, id: Uuid) -> Result<(), DocError> {
        let mut tx = self.begin().await?;
        locks::tree(&mut tx, c.owner())
            .await
            .map_err(DocError::db)?;
        let row = owned_folder(&mut *tx, c, id, Action::Purge).await?;
        if row.state != ItemState::Trashed {
            return Err(DocError::NotFound);
        }
        let removed = purge::purge_folder_tree(&mut tx, c.owner(), id)
            .await
            .map_err(DocError::db)?
            .ok_or(DocError::NotFound)?;
        audit::success(&mut tx, c, "folder.purge", Subject::Folder(id)).await?;
        for doc in &removed.documents {
            audit::success(&mut tx, c, "document.purge", Subject::Document(*doc)).await?;
        }
        tx.commit().await.map_err(DocError::db)?;
        self.release_purged(&removed.documents).await;
        Ok(())
    }

    /// Deletes the bytes of newly `PURGE_PENDING` documents and marks them `PURGED`. The purge is
    /// already committed, so a failure here is logged and left to the purge job, which finishes it.
    async fn release_purged(&self, ids: &[Uuid]) {
        if ids.is_empty() {
            return;
        }
        if let Err(e) = self.try_release_purged(ids).await {
            error!(error = %e, "releasing purged content failed");
        }
    }

    async fn try_release_purged(&self, ids: &[Uuid]) -> Result<(), DocError> {
        let dead = purge::dead_objects_of(&self.db, ids)
            .await
            .map_err(DocError::db)?;
        for object in &dead {
            self.delete_dead_object(object).await?;
        }
        let mut tx = self.begin().await?;
        purge::finish_some(&mut tx, ids)
            .await
            .map_err(DocError::db)?;
        tx.commit().await.map_err(DocError::db)
    }
}
