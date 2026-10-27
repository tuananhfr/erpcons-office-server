use std::cmp::Ordering;

use office_db::{documents, folders, versions};
use time::OffsetDateTime;
use uuid::Uuid;

use super::{Caller, Documents, documents::owned_document, folders::active_folder};
use crate::{
    Action, DocError, Document, Entries, Folder, Page, PageRequest, Version,
    cursor::{self, ChildrenCursor, TrashCursor},
};

impl Documents {
    /// Folders first, then documents, each by name; `None` is the caller's root.
    pub async fn list_children(
        &self,
        c: &Caller<'_>,
        folder: Option<Uuid>,
        page: &PageRequest,
    ) -> Result<Page<Entries>, DocError> {
        if let Some(id) = folder {
            active_folder(&self.db, c, id, Action::Read).await?;
        }
        let limit = usize::try_from(page.limit).unwrap_or(usize::MAX);
        let fetch = i64::from(page.limit) + 1;
        let cursor = page
            .cursor
            .as_deref()
            .map(ChildrenCursor::decode)
            .transpose()?;
        let db = &self.db;
        let mut folder_rows = Vec::new();
        let doc_after = match cursor {
            Some(ChildrenCursor::Document(id)) => {
                let key = documents::sort_key(db, c.owner(), id)
                    .await
                    .map_err(DocError::db)?
                    .ok_or(DocError::InvalidRequest)?;
                Some((key, id))
            }
            folder_cursor => {
                let after = match folder_cursor {
                    Some(ChildrenCursor::Folder(id)) => {
                        let key = folders::sort_key(db, c.owner(), id)
                            .await
                            .map_err(DocError::db)?
                            .ok_or(DocError::InvalidRequest)?;
                        Some((key, id))
                    }
                    _ => None,
                };
                let after_ref = after.as_ref().map(|(k, id)| (k.as_str(), *id));
                folder_rows = folders::list_children(db, c.owner(), folder, after_ref, fetch)
                    .await
                    .map_err(DocError::db)?;
                if folder_rows.len() > limit {
                    folder_rows.truncate(limit);
                    let last = folder_rows.last().map(|f| f.id);
                    return Ok(Page {
                        items: Entries {
                            folders: folder_rows.into_iter().map(Folder::from).collect(),
                            documents: Vec::new(),
                        },
                        next_cursor: last.map(|id| ChildrenCursor::Folder(id).encode()),
                    });
                }
                None
            }
        };
        let room = limit - folder_rows.len();
        let after_ref = doc_after.as_ref().map(|(k, id)| (k.as_str(), *id));
        let mut doc_rows = documents::list_in_folder(
            db,
            c.owner(),
            folder,
            after_ref,
            i64::try_from(room).unwrap_or(i64::MAX) + 1,
        )
        .await
        .map_err(DocError::db)?;
        let next_cursor = if doc_rows.len() > room {
            doc_rows.truncate(room);
            // With no room left, resume after the last folder: the next page starts with documents.
            match (doc_rows.last(), folder_rows.last()) {
                (Some(d), _) => Some(ChildrenCursor::Document(d.id).encode()),
                (None, Some(f)) => Some(ChildrenCursor::Folder(f.id).encode()),
                (None, None) => None,
            }
        } else {
            None
        };
        Ok(Page {
            items: Entries {
                folders: folder_rows.into_iter().map(Folder::from).collect(),
                documents: doc_rows
                    .into_iter()
                    .map(Document::try_from)
                    .collect::<Result<_, _>>()?,
            },
            next_cursor,
        })
    }

    /// Top-level trashed folders and documents, most recently trashed first.
    pub async fn list_trash(
        &self,
        c: &Caller<'_>,
        page: &PageRequest,
    ) -> Result<Page<Entries>, DocError> {
        let limit = usize::try_from(page.limit).unwrap_or(usize::MAX);
        let fetch = i64::from(page.limit) + 1;
        let after = page
            .cursor
            .as_deref()
            .map(TrashCursor::decode)
            .transpose()?
            .map(|t| (t.trashed_at, t.rank, t.id));
        let folder_rows = folders::list_trash(&self.db, c.owner(), after, fetch)
            .await
            .map_err(DocError::db)?;
        let doc_rows = documents::list_trash(&self.db, c.owner(), after, fetch)
            .await
            .map_err(DocError::db)?;
        let mut merged: Vec<TrashItem> = folder_rows
            .into_iter()
            .map(|f| TrashItem::Folder(f.into()))
            .chain(
                doc_rows
                    .into_iter()
                    .map(|d| Document::try_from(d).map(TrashItem::Document))
                    .collect::<Result<Vec<_>, _>>()?,
            )
            .collect();
        merged.sort_by(TrashItem::order);
        let more = merged.len() > limit;
        merged.truncate(limit);
        let next_cursor = more
            .then(|| merged.last().map(|i| i.cursor().encode()))
            .flatten();
        let mut entries = Entries {
            folders: Vec::new(),
            documents: Vec::new(),
        };
        for item in merged {
            match item {
                TrashItem::Folder(f) => entries.folders.push(f),
                TrashItem::Document(d) => entries.documents.push(d),
            }
        }
        Ok(Page {
            items: entries,
            next_cursor,
        })
    }

    /// Versions newest first.
    pub async fn list_versions(
        &self,
        c: &Caller<'_>,
        id: Uuid,
        page: &PageRequest,
    ) -> Result<Page<Vec<Version>>, DocError> {
        owned_document(&self.db, c, id, Action::Read).await?;
        let limit = usize::try_from(page.limit).unwrap_or(usize::MAX);
        let before = page
            .cursor
            .as_deref()
            .map(cursor::decode_version)
            .transpose()?;
        let mut rows = versions::list(&self.db, id, before, i64::from(page.limit) + 1)
            .await
            .map_err(DocError::db)?;
        let more = rows.len() > limit;
        rows.truncate(limit);
        let next_cursor = more
            .then(|| rows.last().map(|v| cursor::encode_version(v.seq)))
            .flatten();
        Ok(Page {
            items: rows.into_iter().map(Version::from).collect(),
            next_cursor,
        })
    }
}

enum TrashItem {
    Folder(Folder),
    Document(Document),
}

impl TrashItem {
    fn key(&self) -> (OffsetDateTime, i16, Uuid) {
        match self {
            Self::Folder(f) => (f.trashed_at.unwrap_or(f.updated_at), 0, f.id),
            Self::Document(d) => (d.trashed_at.unwrap_or(d.updated_at), 1, d.id),
        }
    }

    /// Newest first; at equal times folders, then by id (the order the queries resume in).
    fn order(a: &Self, b: &Self) -> Ordering {
        let (ta, ra, ia) = a.key();
        let (tb, rb, ib) = b.key();
        tb.cmp(&ta).then(ra.cmp(&rb)).then(ia.cmp(&ib))
    }

    fn cursor(&self) -> TrashCursor {
        let (trashed_at, rank, id) = self.key();
        TrashCursor {
            trashed_at,
            rank,
            id,
        }
    }
}
