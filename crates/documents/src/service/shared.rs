//! "Shared with me": documents other people shared with the caller.
use office_db::acl;
use uuid::Uuid;

use super::{Caller, Documents};
use crate::{
    DocError, Document, Page, PageRequest, Role, SharedDocument, SharedList, cursor::SharedCursor,
};

impl Documents {
    /// Most recently shared first, with how many of all are not yet marked seen.
    pub async fn list_shared(
        &self,
        c: &Caller<'_>,
        page: &PageRequest,
    ) -> Result<Page<SharedList>, DocError> {
        let limit = usize::try_from(page.limit).unwrap_or(usize::MAX);
        let after = page
            .cursor
            .as_deref()
            .map(SharedCursor::decode)
            .transpose()?
            .map(|s| (s.shared_at, s.id));
        let mut rows = acl::shared_with(&self.db, c.actor_id(), after, i64::from(page.limit) + 1)
            .await
            .map_err(DocError::db)?;
        let more = rows.len() > limit;
        rows.truncate(limit);
        let next_cursor = more
            .then(|| {
                rows.last().map(|r| {
                    SharedCursor {
                        shared_at: r.shared_at,
                        id: r.grant_id,
                    }
                    .encode()
                })
            })
            .flatten();
        let unseen = acl::unseen_count(&self.db, c.actor_id())
            .await
            .map_err(DocError::db)?;
        let mut documents = Vec::with_capacity(rows.len());
        for row in rows {
            let role = Role::parse_grant(&row.grant_role).ok_or(DocError::Unavailable)?;
            documents.push(SharedDocument {
                document: Document::from_row(
                    row.document,
                    role,
                    row.viewers_can_download,
                    row.editors_can_share,
                )?,
                shared_at: row.shared_at,
                new: row.unseen,
            });
        }
        Ok(Page {
            items: SharedList {
                documents,
                unseen: u64::try_from(unseen).unwrap_or(0),
            },
            next_cursor,
        })
    }

    /// Clears the "new" mark; [`DocError::NotFound`] when the document is not shared with the caller.
    pub async fn mark_shared_seen(&self, c: &Caller<'_>, id: Uuid) -> Result<(), DocError> {
        if acl::mark_seen(&self.db, c.actor_id(), id)
            .await
            .map_err(DocError::db)?
        {
            Ok(())
        } else {
            Err(DocError::NotFound)
        }
    }
}
