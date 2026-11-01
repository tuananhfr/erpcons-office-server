//! Reading a document's comment threads (plan-m5). Owner, editors and commenters see them; viewers
//! and link holders do not (403 for a viewer, who can see the document; 404 for anyone who cannot).
//! The document's owner still reads them in the trash.
use std::collections::HashMap;

use office_db::{ItemState, PgExecutor, comments};
use uuid::Uuid;

use super::{Caller, Documents, access};
use crate::{
    Action, Comment, DocError, PageRequest, Thread, ThreadStatus, Threads, cursor::ThreadCursor,
};

impl Documents {
    /// A page of threads, oldest first, each with its comments.
    pub async fn list_comments(
        &self,
        c: &Caller<'_>,
        id: Uuid,
        status: Option<ThreadStatus>,
        page: &PageRequest,
    ) -> Result<Threads, DocError> {
        let accessed = access::load(&self.db, c, id, Action::ReadComments).await?;
        let after = page
            .cursor
            .as_deref()
            .map(ThreadCursor::decode)
            .transpose()?
            .map(|cursor| cursor.0);
        let limit = i64::from(page.limit);
        let mut rows = comments::list_threads(
            &self.db,
            id,
            status.map(ThreadStatus::as_str),
            after,
            limit + 1,
        )
        .await
        .map_err(DocError::db)?;
        let more = rows.len() > page.limit as usize;
        rows.truncate(page.limit as usize);
        let next_cursor = more
            .then(|| rows.last().map(|t| ThreadCursor(t.id).encode()))
            .flatten();
        let threads = with_comments(&self.db, c, rows).await?;
        Ok(Threads {
            threads,
            next_cursor,
            // Writes to a trashed document answer 404 whatever the role says.
            can_resolve: accessed.row.state == ItemState::Active
                && accessed.allows(c.actor, Action::ModerateComments).is_ok(),
        })
    }

    pub async fn get_comment_thread(
        &self,
        c: &Caller<'_>,
        id: Uuid,
        thread_id: Uuid,
    ) -> Result<Thread, DocError> {
        access::load(&self.db, c, id, Action::ReadComments).await?;
        thread_of(&self.db, c, id, thread_id).await
    }
}

/// One thread of `id` with its comments; `NotFound` when it is not one of that document's.
pub(super) async fn thread_of<'c>(
    db: impl PgExecutor<'c> + Copy,
    c: &Caller<'_>,
    id: Uuid,
    thread_id: Uuid,
) -> Result<Thread, DocError> {
    let row = comments::thread(db, id, thread_id)
        .await
        .map_err(DocError::db)?
        .ok_or(DocError::NotFound)?;
    with_comments(db, c, vec![row])
        .await?
        .pop()
        .ok_or(DocError::NotFound)
}

async fn with_comments<'c>(
    db: impl PgExecutor<'c>,
    c: &Caller<'_>,
    rows: Vec<comments::ThreadRow>,
) -> Result<Vec<Thread>, DocError> {
    let ids: Vec<Uuid> = rows.iter().map(|t| t.id).collect();
    let mut by_thread: HashMap<Uuid, Vec<Comment>> = HashMap::new();
    if !ids.is_empty() {
        for row in comments::comments_of(db, &ids)
            .await
            .map_err(DocError::db)?
        {
            by_thread
                .entry(row.thread_id)
                .or_default()
                .push(Comment::from_row(row, c.actor_id()));
        }
    }
    rows.into_iter()
        .map(|row| {
            let comments = by_thread.remove(&row.id).unwrap_or_default();
            Thread::from_row(row, c.actor_id(), comments)
        })
        .collect()
}
