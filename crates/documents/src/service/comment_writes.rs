//! Writing comments (plan-m5): owner, editors and commenters start threads and reply; only a
//! comment's author edits or deletes it. Not on a trashed document (404, like every change there).
//! Each change locks the document row, so the decision and the caps hold until the commit; after
//! it the room hears `comments.changed`. Audit rows carry the document id only.
use office_db::comments::{self, NewThread as NewThreadRow};
use uuid::Uuid;

use super::{
    Caller, Documents, access,
    audit::{self, Subject},
    comments::thread_of,
};
use crate::{
    Action, Body, Comment, DocError, MAX_COMMENT_ROWS_PER_THREAD, MAX_COMMENTS_PER_THREAD,
    MAX_THREADS_PER_DOCUMENT, NewThread, Thread, check_region, comments_model::words_fit,
};

impl Documents {
    pub async fn create_comment_thread(
        &self,
        c: &Caller<'_>,
        id: Uuid,
        new: NewThread,
    ) -> Result<Thread, DocError> {
        let result = self.create_thread_inner(c, id, new).await;
        self.audit_failure(c, "comment.create", Subject::Document(id), &result)
            .await;
        result
    }

    async fn create_thread_inner(
        &self,
        c: &Caller<'_>,
        id: Uuid,
        new: NewThread,
    ) -> Result<Thread, DocError> {
        if let Some(region) = &new.region {
            check_region(region)?;
        }
        self.ensure_caller(c).await?;
        let mut tx = self.begin().await?;
        let accessed = access::load_for_update(&mut tx, c, id, Action::Comment).await?;
        let version = new.version.unwrap_or(accessed.row.version);
        if !(1..=accessed.row.version).contains(&version) {
            return Err(DocError::InvalidRequest);
        }
        let held = comments::count_threads(&mut *tx, id)
            .await
            .map_err(DocError::db)?;
        if held >= MAX_THREADS_PER_DOCUMENT {
            return Err(DocError::TooManyComments);
        }
        self.check_words(&mut tx, id, new.body.as_str().len())
            .await?;
        let thread_id = Uuid::now_v7();
        let row = NewThreadRow {
            id: thread_id,
            document_id: id,
            anchor: new.anchor.as_str(),
            region: new.region.as_deref(),
            created_version_seq: version,
            created_by: c.actor_id(),
        };
        comments::insert_thread(&mut tx, &row)
            .await
            .map_err(DocError::db)?;
        comments::insert_comment(
            &mut tx,
            Uuid::now_v7(),
            thread_id,
            c.actor_id(),
            new.body.as_str(),
        )
        .await
        .map_err(DocError::db)?;
        if self.mail.enabled {
            self.queue_thread_mail(&mut tx, c, &accessed, thread_id)
                .await?;
        }
        audit::success(&mut tx, c, "comment.create", Subject::Document(id)).await?;
        tx.commit().await.map_err(DocError::db)?;
        self.live_comments(id, Some(thread_id));
        thread_of(&self.db, c, id, thread_id).await
    }

    pub async fn reply_to_thread(
        &self,
        c: &Caller<'_>,
        id: Uuid,
        thread_id: Uuid,
        body: Body,
    ) -> Result<Comment, DocError> {
        let result = self.reply_inner(c, id, thread_id, body).await;
        self.audit_failure(c, "comment.reply", Subject::Document(id), &result)
            .await;
        result
    }

    async fn reply_inner(
        &self,
        c: &Caller<'_>,
        id: Uuid,
        thread_id: Uuid,
        body: Body,
    ) -> Result<Comment, DocError> {
        self.ensure_caller(c).await?;
        let mut tx = self.begin().await?;
        let accessed = access::load_for_update(&mut tx, c, id, Action::Comment).await?;
        if !comments::lock_thread(&mut tx, id, thread_id)
            .await
            .map_err(DocError::db)?
        {
            return Err(DocError::NotFound);
        }
        let (rows, live) = comments::count_comments(&mut *tx, thread_id)
            .await
            .map_err(DocError::db)?;
        if live >= MAX_COMMENTS_PER_THREAD || rows >= MAX_COMMENT_ROWS_PER_THREAD {
            return Err(DocError::TooManyComments);
        }
        self.check_words(&mut tx, id, body.as_str().len()).await?;
        let comment_id = Uuid::now_v7();
        comments::insert_comment(&mut tx, comment_id, thread_id, c.actor_id(), body.as_str())
            .await
            .map_err(DocError::db)?;
        if self.mail.enabled {
            self.queue_reply_mails(&mut tx, c, &accessed, thread_id)
                .await?;
        }
        audit::success(&mut tx, c, "comment.reply", Subject::Document(id)).await?;
        tx.commit().await.map_err(DocError::db)?;
        self.live_comments(id, Some(thread_id));
        self.comment_of(c, thread_id, comment_id).await
    }

    /// Replaces the words of the caller's own comment.
    pub async fn edit_comment(
        &self,
        c: &Caller<'_>,
        id: Uuid,
        thread_id: Uuid,
        comment_id: Uuid,
        body: Body,
    ) -> Result<Comment, DocError> {
        let result = self
            .own_comment_change(c, id, thread_id, comment_id, Some(&body))
            .await;
        self.audit_failure(c, "comment.edit", Subject::Document(id), &result)
            .await;
        result?;
        self.comment_of(c, thread_id, comment_id).await
    }

    /// Deletes the words of the caller's own comment; a thread left without words goes.
    pub async fn delete_comment(
        &self,
        c: &Caller<'_>,
        id: Uuid,
        thread_id: Uuid,
        comment_id: Uuid,
    ) -> Result<(), DocError> {
        let result = self
            .own_comment_change(c, id, thread_id, comment_id, None)
            .await;
        self.audit_failure(c, "comment.delete", Subject::Document(id), &result)
            .await;
        result
    }

    /// Edits (`Some`) or deletes (`None`) a comment of the caller. Someone else's is 403, as the
    /// caller can see it; a deleted one is gone (404).
    async fn own_comment_change(
        &self,
        c: &Caller<'_>,
        id: Uuid,
        thread_id: Uuid,
        comment_id: Uuid,
        body: Option<&Body>,
    ) -> Result<(), DocError> {
        let mut tx = self.begin().await?;
        access::load_for_update(&mut tx, c, id, Action::Comment).await?;
        if !comments::lock_thread(&mut tx, id, thread_id)
            .await
            .map_err(DocError::db)?
        {
            return Err(DocError::NotFound);
        }
        let row = comments::comment(&mut *tx, thread_id, comment_id)
            .await
            .map_err(DocError::db)?
            .filter(|row| row.deleted_at.is_none())
            .ok_or(DocError::NotFound)?;
        if row.author_id != c.actor_id() {
            return Err(DocError::Forbidden);
        }
        let action = match body {
            Some(body) => {
                let old = row.body.as_deref().map_or(0, str::len);
                if let Some(growth) = body.as_str().len().checked_sub(old) {
                    self.check_words(&mut tx, id, growth).await?;
                }
                comments::edit(&mut tx, thread_id, comment_id, body.as_str())
                    .await
                    .map_err(DocError::db)?;
                "comment.edit"
            }
            None => {
                comments::soft_delete(&mut tx, thread_id, comment_id)
                    .await
                    .map_err(DocError::db)?;
                let left = comments::live_comments(&mut *tx, thread_id)
                    .await
                    .map_err(DocError::db)?;
                if left == 0 {
                    comments::delete_thread(&mut tx, id, thread_id)
                        .await
                        .map_err(DocError::db)?;
                }
                "comment.delete"
            }
        };
        audit::success(&mut tx, c, action, Subject::Document(id)).await?;
        tx.commit().await.map_err(DocError::db)?;
        self.live_comments(id, Some(thread_id));
        Ok(())
    }

    /// Refuses words past the document's bound; the document row is locked, so it holds.
    async fn check_words(
        &self,
        conn: &mut office_db::PgConnection,
        id: Uuid,
        adding: usize,
    ) -> Result<(), DocError> {
        let held = comments::body_bytes(&mut *conn, id)
            .await
            .map_err(DocError::db)?;
        words_fit(held, adding)
    }

    async fn comment_of(
        &self,
        c: &Caller<'_>,
        thread_id: Uuid,
        comment_id: Uuid,
    ) -> Result<Comment, DocError> {
        comments::comment(&self.db, thread_id, comment_id)
            .await
            .map_err(DocError::db)?
            .map(|row| Comment::from_row(row, c.actor_id()))
            .ok_or(DocError::NotFound)
    }
}
