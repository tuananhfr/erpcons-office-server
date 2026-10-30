use office_db::{ItemState, documents, locks};
use uuid::Uuid;

use super::{
    Caller, Documents, access,
    audit::{self, Subject},
    folders::{active_folder, restore_target},
};
use crate::{Action, DocError, Document, Name};

/// A rename and/or move; at least one is set. Neither creates a version.
pub struct DocumentPatch {
    pub title: Option<String>,
    /// `Some(None)` moves to the root.
    pub folder_id: Option<Option<Uuid>>,
}

impl Documents {
    pub async fn get_document(&self, c: &Caller<'_>, id: Uuid) -> Result<Document, DocError> {
        access::load(&self.db, c, id, Action::Read)
            .await?
            .document()
    }

    pub async fn update_document(
        &self,
        c: &Caller<'_>,
        id: Uuid,
        patch: DocumentPatch,
    ) -> Result<Document, DocError> {
        let result = self.update_document_inner(c, id, patch).await;
        self.audit_failure(c, "document.update", Subject::Document(id), &result)
            .await;
        result
    }

    async fn update_document_inner(
        &self,
        c: &Caller<'_>,
        id: Uuid,
        patch: DocumentPatch,
    ) -> Result<Document, DocError> {
        if patch.title.is_none() && patch.folder_id.is_none() {
            return Err(DocError::InvalidRequest);
        }
        let title = patch.title.as_deref().map(Name::parse).transpose()?;
        // Folders are the owner's: a move is the owner's alone, a rename also an editor's.
        let action = if patch.folder_id.is_some() {
            Action::Move
        } else {
            Action::Rename
        };
        let mut tx = self.begin().await?;
        if patch.folder_id.is_some() {
            // Only the owner gets past the check below, so this is the owner's tree.
            locks::tree(&mut tx, c.actor_id())
                .await
                .map_err(DocError::db)?;
        }
        // The row lock makes the decision hold until the commit, as for every other write.
        access::load_for_update(&mut tx, c, id, action).await?;
        if let Some(Some(folder)) = patch.folder_id {
            active_folder(&mut *tx, c, folder, Action::Write).await?;
        }
        let change = documents::DocumentChange {
            title: title.as_ref().map(Name::as_str),
            folder_id: patch.folder_id,
        };
        if !documents::update(&mut *tx, id, &change)
            .await
            .map_err(DocError::db)?
        {
            return Err(DocError::NotFound);
        }
        audit::success(&mut tx, c, "document.update", Subject::Document(id)).await?;
        let accessed = access::load_active(&mut *tx, c, id, Action::Read).await?;
        tx.commit().await.map_err(DocError::db)?;
        accessed.document()
    }

    /// Moves the document to the trash; trashing it again is a no-op.
    pub async fn trash_document(&self, c: &Caller<'_>, id: Uuid) -> Result<(), DocError> {
        let result = self.trash_document_inner(c, id).await;
        self.audit_failure(c, "document.trash", Subject::Document(id), &result)
            .await;
        result
    }

    async fn trash_document_inner(&self, c: &Caller<'_>, id: Uuid) -> Result<(), DocError> {
        let mut tx = self.begin().await?;
        let accessed = access::load(&mut *tx, c, id, Action::Trash).await?;
        if accessed.row.state == ItemState::Trashed {
            return Ok(());
        }
        if !documents::trash(&mut *tx, id).await.map_err(DocError::db)? {
            // Trashed or purged by someone else meanwhile.
            return Ok(());
        }
        audit::success(&mut tx, c, "document.trash", Subject::Document(id)).await?;
        tx.commit().await.map_err(DocError::db)
    }

    /// Restores the document, into the root when its folder is gone or trashed. Restoring an
    /// active document returns it unchanged.
    pub async fn restore_document(&self, c: &Caller<'_>, id: Uuid) -> Result<Document, DocError> {
        let result = self.restore_document_inner(c, id).await;
        self.audit_failure(c, "document.restore", Subject::Document(id), &result)
            .await;
        result
    }

    async fn restore_document_inner(&self, c: &Caller<'_>, id: Uuid) -> Result<Document, DocError> {
        let mut tx = self.begin().await?;
        // Restoring is the owner's alone, so this is the owner's tree.
        locks::tree(&mut tx, c.actor_id())
            .await
            .map_err(DocError::db)?;
        let accessed = access::load(&mut *tx, c, id, Action::Restore).await?;
        if accessed.row.state == ItemState::Active {
            return accessed.document();
        }
        let folder = restore_target(&mut tx, c, accessed.row.folder_id).await?;
        if !documents::restore(&mut *tx, id, folder)
            .await
            .map_err(DocError::db)?
        {
            return Err(DocError::NotFound);
        }
        audit::success(&mut tx, c, "document.restore", Subject::Document(id)).await?;
        let accessed = access::load(&mut *tx, c, id, Action::Read).await?;
        tx.commit().await.map_err(DocError::db)?;
        accessed.document()
    }
}
