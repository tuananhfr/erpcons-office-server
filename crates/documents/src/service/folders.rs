use office_db::{ItemState, PgConnection, PgExecutor, folders, is_unique_violation, locks};
use uuid::Uuid;

use super::{
    Caller, Documents,
    audit::{self, Subject},
};
use crate::{Action, DocError, Folder, FolderPath, Name, Resource, authorize};

/// A rename and/or move; at least one is set.
pub struct FolderPatch {
    pub name: Option<String>,
    /// `Some(None)` moves to the root.
    pub parent_id: Option<Option<Uuid>>,
}

/// Loads a folder the caller may act on; anything else is [`DocError::NotFound`].
pub(super) async fn owned_folder(
    db: impl PgExecutor<'_>,
    c: &Caller<'_>,
    id: Uuid,
    action: Action,
) -> Result<folders::FolderRow, DocError> {
    let row = folders::get(db, id)
        .await
        .map_err(DocError::db)?
        .ok_or(DocError::NotFound)?;
    authorize(
        c.actor,
        Resource::Folder {
            owner_id: &row.owner_id,
        },
        action,
    )?;
    Ok(row)
}

/// As [`owned_folder`], and the folder must be active (a trashed folder accepts no new content).
pub(super) async fn active_folder(
    db: impl PgExecutor<'_>,
    c: &Caller<'_>,
    id: Uuid,
    action: Action,
) -> Result<folders::FolderRow, DocError> {
    let row = owned_folder(db, c, id, action).await?;
    if row.state != ItemState::Active {
        return Err(DocError::NotFound);
    }
    Ok(row)
}

fn name_conflict(err: office_db::Error) -> DocError {
    if is_unique_violation(&err, folders::NAME_INDEX) {
        DocError::NameTaken
    } else {
        DocError::db(err)
    }
}

impl Documents {
    /// An active folder with its path; a trashed one is as missing as another owner's.
    pub async fn get_folder(&self, c: &Caller<'_>, id: Uuid) -> Result<FolderPath, DocError> {
        let folder = active_folder(&self.db, c, id, Action::Read).await?;
        let rows = folders::ancestors(&self.db, c.owner(), id)
            .await
            .map_err(DocError::db)?;
        let mut ancestors = Vec::with_capacity(rows.len());
        for row in rows {
            authorize(
                c.actor,
                Resource::Folder {
                    owner_id: &row.owner_id,
                },
                Action::Read,
            )?;
            ancestors.push(row.into());
        }
        Ok(FolderPath {
            folder: folder.into(),
            ancestors,
        })
    }

    pub async fn create_folder(
        &self,
        c: &Caller<'_>,
        parent_id: Option<Uuid>,
        name: &str,
    ) -> Result<Folder, DocError> {
        let result = self.create_folder_inner(c, parent_id, name).await;
        self.audit_failure(c, "folder.create", Subject::None, &result)
            .await;
        result
    }

    async fn create_folder_inner(
        &self,
        c: &Caller<'_>,
        parent_id: Option<Uuid>,
        name: &str,
    ) -> Result<Folder, DocError> {
        let name = Name::parse(name)?;
        let mut tx = self.begin().await?;
        self.touch_actor(&mut tx, c).await?;
        locks::tree(&mut tx, c.owner())
            .await
            .map_err(DocError::db)?;
        if let Some(parent) = parent_id {
            active_folder(&mut *tx, c, parent, Action::Write).await?;
        }
        let key = name.key();
        let new = folders::NewFolder {
            id: Uuid::now_v7(),
            owner_id: c.owner(),
            parent_id,
            name: name.as_str(),
            name_key: &key,
        };
        let row = folders::insert(&mut *tx, &new)
            .await
            .map_err(name_conflict)?;
        audit::success(&mut tx, c, "folder.create", Subject::Folder(new.id)).await?;
        tx.commit().await.map_err(DocError::db)?;
        Ok(row.into())
    }

    pub async fn update_folder(
        &self,
        c: &Caller<'_>,
        id: Uuid,
        patch: FolderPatch,
    ) -> Result<Folder, DocError> {
        let result = self.update_folder_inner(c, id, patch).await;
        self.audit_failure(c, "folder.update", Subject::Folder(id), &result)
            .await;
        result
    }

    async fn update_folder_inner(
        &self,
        c: &Caller<'_>,
        id: Uuid,
        patch: FolderPatch,
    ) -> Result<Folder, DocError> {
        if patch.name.is_none() && patch.parent_id.is_none() {
            return Err(DocError::InvalidRequest);
        }
        let name = patch.name.as_deref().map(Name::parse).transpose()?;
        let key = name.as_ref().map(Name::key);
        let mut tx = self.begin().await?;
        locks::tree(&mut tx, c.owner())
            .await
            .map_err(DocError::db)?;
        active_folder(&mut *tx, c, id, Action::Write).await?;
        if let Some(Some(parent)) = patch.parent_id {
            if parent == id {
                return Err(DocError::FolderCycle);
            }
            active_folder(&mut *tx, c, parent, Action::Write).await?;
            if folders::is_ancestor_or_self(&mut *tx, id, parent)
                .await
                .map_err(DocError::db)?
            {
                return Err(DocError::FolderCycle);
            }
        }
        let change = folders::FolderChange {
            name: name.as_ref().map(Name::as_str).zip(key.as_deref()),
            parent_id: patch.parent_id,
        };
        let row = folders::update(&mut *tx, id, &change)
            .await
            .map_err(name_conflict)?
            .ok_or(DocError::NotFound)?;
        audit::success(&mut tx, c, "folder.update", Subject::Folder(id)).await?;
        tx.commit().await.map_err(DocError::db)?;
        Ok(row.into())
    }

    /// Trashes the folder and everything in it. Trashing it again is a no-op.
    pub async fn trash_folder(&self, c: &Caller<'_>, id: Uuid) -> Result<(), DocError> {
        let result = self.trash_folder_inner(c, id).await;
        self.audit_failure(c, "folder.trash", Subject::Folder(id), &result)
            .await;
        result
    }

    async fn trash_folder_inner(&self, c: &Caller<'_>, id: Uuid) -> Result<(), DocError> {
        let mut tx = self.begin().await?;
        locks::tree(&mut tx, c.owner())
            .await
            .map_err(DocError::db)?;
        let row = owned_folder(&mut *tx, c, id, Action::Trash).await?;
        match row.state {
            ItemState::Active => {}
            ItemState::Trashed => return Ok(()),
            _ => return Err(DocError::NotFound),
        }
        folders::trash_tree(&mut tx, c.owner(), id)
            .await
            .map_err(DocError::db)?;
        audit::success(&mut tx, c, "folder.trash", Subject::Folder(id)).await?;
        tx.commit().await.map_err(DocError::db)
    }

    /// Restores the folder and what was trashed with it, into the root when its former parent is
    /// gone or trashed. Restoring an active folder returns it unchanged.
    pub async fn restore_folder(&self, c: &Caller<'_>, id: Uuid) -> Result<Folder, DocError> {
        let result = self.restore_folder_inner(c, id).await;
        self.audit_failure(c, "folder.restore", Subject::Folder(id), &result)
            .await;
        result
    }

    async fn restore_folder_inner(&self, c: &Caller<'_>, id: Uuid) -> Result<Folder, DocError> {
        let mut tx = self.begin().await?;
        locks::tree(&mut tx, c.owner())
            .await
            .map_err(DocError::db)?;
        let row = owned_folder(&mut *tx, c, id, Action::Restore).await?;
        match row.state {
            ItemState::Trashed => {}
            ItemState::Active => return Ok(row.into()),
            _ => return Err(DocError::NotFound),
        }
        let parent = restore_target(&mut tx, c, row.parent_id).await?;
        let restored = folders::restore_tree(&mut tx, c.owner(), id, parent)
            .await
            .map_err(name_conflict)?
            .ok_or(DocError::NotFound)?;
        audit::success(&mut tx, c, "folder.restore", Subject::Folder(id)).await?;
        tx.commit().await.map_err(DocError::db)?;
        Ok(restored.into())
    }
}

/// Where restored content goes: its former folder while that is active, else the root.
pub(super) async fn restore_target(
    conn: &mut PgConnection,
    c: &Caller<'_>,
    former: Option<Uuid>,
) -> Result<Option<Uuid>, DocError> {
    let Some(former) = former else {
        return Ok(None);
    };
    match active_folder(conn, c, former, Action::Write).await {
        Ok(_) => Ok(Some(former)),
        Err(DocError::NotFound) => Ok(None),
        Err(e) => Err(e),
    }
}
