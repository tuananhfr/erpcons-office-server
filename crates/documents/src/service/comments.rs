//! Reading a document's comment threads (plan-m5). Everyone who can open the document reads them:
//! owner, editors, commenters, viewers and holders of the public link, who see nobody's name (as in
//! the live room). The document's owner still reads them in the trash; anyone who cannot open the
//! document gets 404.
use std::collections::HashMap;

use office_db::{ItemState, PgExecutor, comments};
use uuid::Uuid;

use super::{Caller, Documents, access, audit};
use crate::{
    Action, Comment, DocError, LinkHash, PageRequest, Thread, ThreadStatus, Threads,
    cursor::ThreadCursor,
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
        let (threads, next_cursor) = self.thread_page(id, c.actor_id(), status, page).await?;
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

    /// As [`Self::list_comments`] for a holder of the public link: anonymous, read-only, audited
    /// as a link view.
    pub async fn list_link_comments(
        &self,
        hash: &LinkHash,
        id: Uuid,
        status: Option<ThreadStatus>,
        page: &PageRequest,
        trace_id: &str,
    ) -> Result<Threads, DocError> {
        access::load_link(&self.db, hash, id, Action::ReadComments).await?;
        let (threads, next_cursor) = self.thread_page(id, "", status, page).await?;
        audit::link(&self.db, "link.comments", id, trace_id).await;
        Ok(Threads {
            threads: threads.into_iter().map(Thread::anonymous).collect(),
            next_cursor,
            can_resolve: false,
        })
    }

    /// As [`Self::get_comment_thread`] for a holder of the public link.
    pub async fn get_link_comment_thread(
        &self,
        hash: &LinkHash,
        id: Uuid,
        thread_id: Uuid,
        trace_id: &str,
    ) -> Result<Thread, DocError> {
        access::load_link(&self.db, hash, id, Action::ReadComments).await?;
        let thread = thread_by(&self.db, "", id, thread_id).await?;
        audit::link(&self.db, "link.comments", id, trace_id).await;
        Ok(thread.anonymous())
    }

    /// One page of `id`'s threads as `caller` (an actor id, `""` for nobody) sees them.
    async fn thread_page(
        &self,
        id: Uuid,
        caller: &str,
        status: Option<ThreadStatus>,
        page: &PageRequest,
    ) -> Result<(Vec<Thread>, Option<String>), DocError> {
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
        let threads = with_comments(&self.db, caller, rows).await?;
        Ok((threads, next_cursor))
    }
}

/// One thread of `id` with its comments; `NotFound` when it is not one of that document's.
pub(super) async fn thread_of<'c>(
    db: impl PgExecutor<'c> + Copy,
    c: &Caller<'_>,
    id: Uuid,
    thread_id: Uuid,
) -> Result<Thread, DocError> {
    thread_by(db, c.actor_id(), id, thread_id).await
}

async fn thread_by<'c>(
    db: impl PgExecutor<'c> + Copy,
    caller: &str,
    id: Uuid,
    thread_id: Uuid,
) -> Result<Thread, DocError> {
    let row = comments::thread(db, id, thread_id)
        .await
        .map_err(DocError::db)?
        .ok_or(DocError::NotFound)?;
    with_comments(db, caller, vec![row])
        .await?
        .pop()
        .ok_or(DocError::NotFound)
}

async fn with_comments<'c>(
    db: impl PgExecutor<'c>,
    caller: &str,
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
                .push(Comment::from_row(row, caller));
        }
    }
    rows.into_iter()
        .map(|row| {
            let comments = by_thread.remove(&row.id).unwrap_or_default();
            Thread::from_row(row, caller, comments)
        })
        .collect()
}
