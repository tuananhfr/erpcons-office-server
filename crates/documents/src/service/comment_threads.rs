//! Whole threads (plan-m5): owner and editors resolve, reopen and delete them; owner, editors and
//! commenters report where their editor placed threads again (`orphaned` when it could not).
use office_db::comments::{self, AnchorChange as AnchorChangeRow};
use uuid::Uuid;

use super::{
    Caller, Documents, access,
    audit::{self, Subject},
    comments::thread_of,
};
use crate::{
    Action, AnchorChange, DocError, MAX_ANCHOR_CHANGES, Thread, ThreadStatus, check_region,
};

/// What happens to a thread.
#[derive(Clone, Copy)]
enum Moderation {
    Resolve,
    Reopen,
    Delete,
}

impl Moderation {
    fn action(self) -> &'static str {
        match self {
            Self::Resolve => "comment.resolve",
            Self::Reopen => "comment.reopen",
            Self::Delete => "comment.thread_delete",
        }
    }
}

impl Documents {
    /// Marks the thread resolved; resolving it again changes nothing.
    pub async fn resolve_thread(
        &self,
        c: &Caller<'_>,
        id: Uuid,
        thread_id: Uuid,
    ) -> Result<Thread, DocError> {
        self.moderate(c, id, thread_id, Moderation::Resolve).await?;
        thread_of(&self.db, c, id, thread_id).await
    }

    /// Opens a resolved thread again; an open one stays as it is.
    pub async fn reopen_thread(
        &self,
        c: &Caller<'_>,
        id: Uuid,
        thread_id: Uuid,
    ) -> Result<Thread, DocError> {
        self.moderate(c, id, thread_id, Moderation::Reopen).await?;
        thread_of(&self.db, c, id, thread_id).await
    }

    /// Deletes the thread with all its comments.
    pub async fn delete_thread(
        &self,
        c: &Caller<'_>,
        id: Uuid,
        thread_id: Uuid,
    ) -> Result<(), DocError> {
        self.moderate(c, id, thread_id, Moderation::Delete).await
    }

    async fn moderate(
        &self,
        c: &Caller<'_>,
        id: Uuid,
        thread_id: Uuid,
        what: Moderation,
    ) -> Result<(), DocError> {
        let result = self.moderate_inner(c, id, thread_id, what).await;
        self.audit_failure(c, what.action(), Subject::Document(id), &result)
            .await;
        result
    }

    async fn moderate_inner(
        &self,
        c: &Caller<'_>,
        id: Uuid,
        thread_id: Uuid,
        what: Moderation,
    ) -> Result<(), DocError> {
        self.ensure_caller(c).await?;
        let mut tx = self.begin().await?;
        access::load_for_update(&mut tx, c, id, Action::ModerateComments).await?;
        if !comments::lock_thread(&mut tx, id, thread_id)
            .await
            .map_err(DocError::db)?
        {
            return Err(DocError::NotFound);
        }
        let changed = match what {
            Moderation::Resolve => comments::resolve(&mut tx, id, thread_id, c.actor_id()).await,
            Moderation::Reopen => comments::reopen(&mut tx, id, thread_id).await,
            Moderation::Delete => comments::delete_thread(&mut tx, id, thread_id).await,
        }
        .map_err(DocError::db)?;
        if !changed {
            return Ok(());
        }
        audit::success(&mut tx, c, what.action(), Subject::Document(id)).await?;
        tx.commit().await.map_err(DocError::db)?;
        self.live_comments(id, Some(thread_id));
        Ok(())
    }

    /// Applies what the caller's editor found when it placed threads again; threads that are not
    /// this document's (or no longer exist) are skipped. Returns how many changed.
    pub async fn change_anchors(
        &self,
        c: &Caller<'_>,
        id: Uuid,
        changes: Vec<AnchorChange>,
    ) -> Result<u32, DocError> {
        let result = self.change_anchors_inner(c, id, changes).await;
        self.audit_failure(c, "comment.anchors", Subject::Document(id), &result)
            .await;
        result
    }

    async fn change_anchors_inner(
        &self,
        c: &Caller<'_>,
        id: Uuid,
        changes: Vec<AnchorChange>,
    ) -> Result<u32, DocError> {
        if changes.is_empty() || changes.len() > MAX_ANCHOR_CHANGES {
            return Err(DocError::InvalidRequest);
        }
        for change in &changes {
            if change.status == Some(ThreadStatus::Resolved) {
                return Err(DocError::InvalidRequest);
            }
            if let Some(Some(region)) = &change.region {
                check_region(region)?;
            }
        }
        let mut tx = self.begin().await?;
        access::load_for_update(&mut tx, c, id, Action::Comment).await?;
        let mut changed = 0;
        for change in &changes {
            let row = AnchorChangeRow {
                thread_id: change.thread_id,
                anchor: change.anchor.as_ref().map(|a| a.as_str()),
                region: change.region.as_ref().map(Option::as_deref),
                status: change.status.map(ThreadStatus::as_str),
            };
            if comments::change_anchor(&mut tx, id, &row)
                .await
                .map_err(DocError::db)?
            {
                changed += 1;
            }
        }
        if changed == 0 {
            return Ok(0);
        }
        audit::success(&mut tx, c, "comment.anchors", Subject::Document(id)).await?;
        tx.commit().await.map_err(DocError::db)?;
        self.live_comments(id, None);
        Ok(changed)
    }
}
