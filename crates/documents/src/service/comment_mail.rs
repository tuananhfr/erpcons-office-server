//! Comment mail (plan v2 §5, plan-m5): a reply reaches the others who wrote in the thread; new
//! threads on a document reach its owner, gathered into one mail per 15 minutes. Mails say where
//! to look, never what was written, and name the title only when the server is configured to.
//! Recipients who may not read the comments get none, checked when queued and again when due.
use async_trait::async_trait;
use office_db::{ItemState, PgConnection, actor_emails, comments, locks, mail_outbox};
use office_mail::{
    CommentMails, CommentNotice, KIND_COMMENT_REPLY, KIND_COMMENT_THREADS, SendError,
};
use uuid::Uuid;

use super::{
    Caller, Documents,
    access::Accessed,
    invite_mail::{WINDOW_SECS, remaining},
};
use crate::{Action, DocError, Resource, Role, authorize};

/// New threads of one document wait this long for the others that follow.
const THREADS_BATCH_SECS: i64 = 15 * 60;
/// A reply mail waits this long, so a quick exchange makes one mail per person, not one each.
const REPLY_BATCH_SECS: i64 = 2 * 60;
/// Comment mails one address receives per rolling day, from all documents together.
const COMMENT_MAILS_PER_RECIPIENT: u32 = 50;
const COMMENT_KINDS: [&str; 2] = [KIND_COMMENT_REPLY, KIND_COMMENT_THREADS];

/// Whether someone with `grant` (or none) on a document owned by `owner_id` may read its comments.
fn reads_comments(actor_id: &str, owner_id: &str, grant: Option<&str>, accessed: Shares) -> bool {
    let role = Role::of_id(Some(actor_id), owner_id, grant.and_then(Role::parse_grant));
    let resource = Resource::Document {
        role,
        trashed: accessed.trashed,
        editors_can_share: accessed.editors_can_share,
    };
    authorize(None, resource, Action::ReadComments).is_ok()
}

/// What of the document, besides roles, an access decision reads.
#[derive(Clone, Copy)]
struct Shares {
    trashed: bool,
    editors_can_share: bool,
}

impl Documents {
    /// Queues the owner's mail about new threads unless one is already waiting (it will count this
    /// thread too when it is due).
    pub(super) async fn queue_thread_mail(
        &self,
        conn: &mut PgConnection,
        c: &Caller<'_>,
        accessed: &Accessed,
        thread_id: Uuid,
    ) -> Result<(), DocError> {
        let owner = accessed.owner_id();
        let document = accessed.row.id;
        if owner == c.actor_id() {
            return Ok(());
        }
        let waiting =
            mail_outbox::comment_pending(&mut *conn, KIND_COMMENT_THREADS, owner, document, None)
                .await
                .map_err(DocError::db)?;
        if waiting {
            return Ok(());
        }
        let Some(email) = actor_emails::email_of(&mut *conn, owner)
            .await
            .map_err(DocError::db)?
        else {
            return Ok(());
        };
        let since = comments::thread(&mut *conn, document, thread_id)
            .await
            .map_err(DocError::db)?
            .map(|t| t.created_at);
        let mut allowance = self.comment_mail_allowance(conn, c).await?;
        if !self.recipient_allows(conn, &email, &mut allowance).await? {
            return Ok(());
        }
        let notice = CommentNotice {
            document_id: document,
            kind: accessed.row.kind.clone(),
            thread_id,
            recipient_id: owner.to_owned(),
            replier_name: None,
            since,
            title: self.mail.include_title.then(|| accessed.row.title.clone()),
        };
        let mail = mail_outbox::NewMail {
            kind: KIND_COMMENT_THREADS,
            sender_id: Some(c.actor_id()),
            to_email: &email,
            payload: &notice.to_json(),
        };
        mail_outbox::enqueue_in(conn, &mail, THREADS_BATCH_SECS)
            .await
            .map_err(DocError::db)
    }

    /// Queues a mail to everyone else who wrote in the thread and may still read it, unless one to
    /// them about this thread is already waiting.
    pub(super) async fn queue_reply_mails(
        &self,
        conn: &mut PgConnection,
        c: &Caller<'_>,
        accessed: &Accessed,
        thread_id: Uuid,
    ) -> Result<(), DocError> {
        let document = accessed.row.id;
        let shares = Shares {
            trashed: false,
            editors_can_share: accessed.editors_can_share,
        };
        let people = comments::participants(&mut *conn, thread_id, c.actor_id())
            .await
            .map_err(DocError::db)?;
        if people.is_empty() {
            return Ok(());
        }
        let mut allowance = self.comment_mail_allowance(conn, c).await?;
        for person in people {
            if allowance == 0 {
                break;
            }
            let Some(email) = person.email.as_deref() else {
                continue;
            };
            let grant = person.grant_role.as_deref();
            if !reads_comments(&person.actor_id, accessed.owner_id(), grant, shares) {
                continue;
            }
            let waiting = mail_outbox::comment_pending(
                &mut *conn,
                KIND_COMMENT_REPLY,
                &person.actor_id,
                document,
                Some(thread_id),
            )
            .await
            .map_err(DocError::db)?;
            if waiting {
                continue;
            }
            if !self.recipient_allows(conn, email, &mut allowance).await? {
                continue;
            }
            let notice = CommentNotice {
                document_id: document,
                kind: accessed.row.kind.clone(),
                thread_id,
                recipient_id: person.actor_id.clone(),
                replier_name: Some(c.actor.display_name.clone()),
                since: None,
                title: self.mail.include_title.then(|| accessed.row.title.clone()),
            };
            let mail = mail_outbox::NewMail {
                kind: KIND_COMMENT_REPLY,
                sender_id: Some(c.actor_id()),
                to_email: email,
                payload: &notice.to_json(),
            };
            mail_outbox::enqueue_in(conn, &mail, REPLY_BATCH_SECS)
                .await
                .map_err(DocError::db)?;
        }
        Ok(())
    }

    /// Mails the caller may still cause: the overall and per-sender daily caps shared with
    /// invitations. Takes the mail caps lock until the transaction ends.
    async fn comment_mail_allowance(
        &self,
        conn: &mut PgConnection,
        c: &Caller<'_>,
    ) -> Result<u32, DocError> {
        locks::mail_caps(conn).await.map_err(DocError::db)?;
        let overall = mail_outbox::queued_since(&mut *conn, WINDOW_SECS)
            .await
            .map_err(DocError::db)?;
        let sent = mail_outbox::caused_since(&mut *conn, c.actor_id(), WINDOW_SECS)
            .await
            .map_err(DocError::db)?;
        Ok(
            remaining(self.mail.global_daily_cap, overall)
                .min(remaining(self.mail.daily_cap, sent)),
        )
    }

    /// Whether one more comment mail may go to `email` within the allowance and its own daily cap;
    /// spends the allowance when it may.
    async fn recipient_allows(
        &self,
        conn: &mut PgConnection,
        email: &str,
        allowance: &mut u32,
    ) -> Result<bool, DocError> {
        if *allowance == 0 {
            return Ok(false);
        }
        let received =
            mail_outbox::received_of_kinds_since(&mut *conn, email, &COMMENT_KINDS, WINDOW_SECS)
                .await
                .map_err(DocError::db)?;
        if remaining(COMMENT_MAILS_PER_RECIPIENT, received) == 0 {
            return Ok(false);
        }
        *allowance -= 1;
        Ok(true)
    }

    /// See [`CommentMails::still_due`].
    async fn comment_mail_due(
        &self,
        kind: &str,
        notice: &CommentNotice,
    ) -> Result<Option<u32>, DocError> {
        let row = office_db::access::of_actor(&self.db, notice.document_id, &notice.recipient_id)
            .await
            .map_err(DocError::db)?;
        let Some(row) = row else {
            return Ok(None);
        };
        // A trashed document's comments are its owner's to read, but there is nothing to act on.
        if row.document.state != ItemState::Active {
            return Ok(None);
        }
        let shares = Shares {
            trashed: false,
            editors_can_share: row.editors_can_share,
        };
        let grant = row.grant_role.as_deref();
        if !reads_comments(&notice.recipient_id, &row.document.owner_id, grant, shares) {
            return Ok(None);
        }
        match kind {
            KIND_COMMENT_REPLY => {
                let thread = comments::thread(&self.db, notice.document_id, notice.thread_id)
                    .await
                    .map_err(DocError::db)?;
                Ok(thread.map(|_| 1))
            }
            KIND_COMMENT_THREADS => {
                let Some(since) = notice.since else {
                    return Ok(None);
                };
                let count = comments::new_threads_since(
                    &self.db,
                    notice.document_id,
                    &row.document.owner_id,
                    since,
                )
                .await
                .map_err(DocError::db)?;
                Ok(Some(u32::try_from(count).unwrap_or(u32::MAX)))
            }
            _ => Ok(None),
        }
    }
}

#[async_trait]
impl CommentMails for Documents {
    async fn still_due(
        &self,
        kind: &str,
        notice: &CommentNotice,
    ) -> Result<Option<u32>, SendError> {
        self.comment_mail_due(kind, notice)
            .await
            .map_err(|_| SendError::Transient)
    }
}
