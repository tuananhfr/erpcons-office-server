//! Queuing "shared with you" mail within its caps: per sender, per recipient and overall in a
//! rolling day, and only from a sharer whose email PI verified, so every mail names a real address.
use office_db::{PgConnection, actor_emails, locks, mail_outbox};
use office_mail::{KIND_SHARED, PER_RECIPIENT_DAILY_CAP, SharedNotice};

use super::{Caller, Documents, access::Accessed};
use crate::{DocError, Email, Role};

/// The caps' rolling window; the outbox keeps sent rows longer than this.
const WINDOW_SECS: i64 = 24 * 3600;

/// What the mail says besides who sends it.
pub(super) struct Invite<'a> {
    pub role: Role,
    pub message: Option<&'a str>,
}

impl Documents {
    /// Queues one mail per recipient while the caps allow; returns how many were not queued.
    pub(super) async fn queue_invites(
        &self,
        conn: &mut PgConnection,
        c: &Caller<'_>,
        accessed: &Accessed,
        invite: &Invite<'_>,
        recipients: &[&Email],
    ) -> Result<u32, DocError> {
        let all = u32::try_from(recipients.len()).unwrap_or(u32::MAX);
        let Some(sender_email) = actor_emails::email_of(&mut *conn, c.actor_id())
            .await
            .map_err(DocError::db)?
        else {
            return Ok(all);
        };
        locks::mail_caps(conn).await.map_err(DocError::db)?;
        let overall = mail_outbox::queued_since(&mut *conn, WINDOW_SECS)
            .await
            .map_err(DocError::db)?;
        let sent = mail_outbox::caused_since(&mut *conn, c.actor_id(), WINDOW_SECS)
            .await
            .map_err(DocError::db)?;
        let mut allowance = remaining(self.mail.global_daily_cap, overall)
            .min(remaining(self.mail.daily_cap, sent));
        let notice = SharedNotice {
            document_id: accessed.row.id,
            kind: accessed.row.kind.clone(),
            role: invite.role.as_str().to_owned(),
            sharer_name: c.actor.display_name.clone(),
            sharer_email: Some(sender_email),
            message: invite.message.map(str::to_owned),
            title: self.mail.include_title.then(|| accessed.row.title.clone()),
        };
        let payload = notice.to_json();
        let mut queued = 0;
        for email in recipients {
            if allowance == 0 {
                break;
            }
            let received = mail_outbox::received_since(&mut *conn, email.as_str(), WINDOW_SECS)
                .await
                .map_err(DocError::db)?;
            if remaining(PER_RECIPIENT_DAILY_CAP, received) == 0 {
                continue;
            }
            let mail = mail_outbox::NewMail {
                kind: KIND_SHARED,
                sender_id: Some(c.actor_id()),
                to_email: email.as_str(),
                payload: &payload,
            };
            mail_outbox::enqueue(conn, &mail)
                .await
                .map_err(DocError::db)?;
            allowance -= 1;
            queued += 1;
        }
        Ok(all - queued)
    }
}

fn remaining(cap: u32, used: i64) -> u32 {
    let used = u32::try_from(used.max(0)).unwrap_or(u32::MAX);
    cap.saturating_sub(used)
}
