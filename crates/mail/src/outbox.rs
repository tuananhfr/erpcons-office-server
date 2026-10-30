//! The job that empties `mail_outbox`: due mails are leased, sent, then marked sent or rescheduled with
//! backoff; a mail still unsent 24 h after it was queued, or refused for good, is given up.
use std::time::Duration;

use office_db::{PgPool, mail_outbox};
use tracing::{error, warn};

use uuid::Uuid;

use crate::{
    KIND_SHARED, KIND_SIGNUP_EXISTING, KIND_SIGNUP_VERIFY, MailTransport, SendError, SharedNotice,
    SignupNotice, SignupTokens, render_shared, render_signup_existing, render_signup_verify,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OutboxConfig {
    /// Mails leased per round.
    pub batch: i64,
    /// A leased mail is free again after this, should its sender die mid-send.
    pub lease: Duration,
    /// First retry delay; it doubles per attempt up to `max_backoff`.
    pub first_backoff: Duration,
    pub max_backoff: Duration,
    /// Unsent this long after queuing: given up (plan v2 §5: 24 h).
    pub give_up_after: Duration,
    /// Sent and given-up rows are deleted this long after queuing; they hold addresses.
    pub keep_done: Duration,
}

impl Default for OutboxConfig {
    fn default() -> Self {
        Self {
            batch: 20,
            lease: Duration::from_secs(300),
            first_backoff: Duration::from_secs(60),
            max_backoff: Duration::from_secs(3600),
            give_up_after: Duration::from_secs(24 * 3600),
            keep_done: Duration::from_secs(7 * 24 * 3600),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OutboxReport {
    pub sent: u64,
    pub retried: u64,
    pub failed: u64,
    /// Verification mails of sign-ups that were replaced, verified or expired before sending.
    pub dropped: u64,
    pub deleted: u64,
}

/// What became of one mail that did not fail.
enum Delivery {
    Sent,
    /// Nothing to send any more.
    Dropped,
}

fn secs(d: Duration) -> i64 {
    i64::try_from(d.as_secs()).unwrap_or(i64::MAX)
}

/// Delay before attempt `attempts + 1`.
fn backoff(config: &OutboxConfig, attempts: i32) -> Duration {
    let doublings = u32::try_from(attempts.saturating_sub(1))
        .unwrap_or(0)
        .min(20);
    config
        .first_backoff
        .saturating_mul(1 << doublings)
        .min(config.max_backoff)
}

fn unknown(mail_id: Uuid) -> Result<Delivery, SendError> {
    error!(mail_id = %mail_id, "outbox row of an unknown kind or payload");
    Err(SendError::Permanent)
}

async fn deliver(
    mail: &mail_outbox::OutboxRow,
    transport: &dyn MailTransport,
    public_origin: &str,
    signups: Option<&dyn SignupTokens>,
) -> Result<Delivery, SendError> {
    let rendered = match mail.kind.as_str() {
        KIND_SHARED => match SharedNotice::from_json(&mail.payload) {
            Some(notice) => render_shared(&notice, public_origin),
            None => return unknown(mail.id),
        },
        KIND_SIGNUP_VERIFY => {
            let (Some(notice), Some(signups)) = (SignupNotice::from_json(&mail.payload), signups)
            else {
                return unknown(mail.id);
            };
            match signups.issue(notice.signup_id).await? {
                Some(issued) => {
                    render_signup_verify(&issued.display_name, &issued.token, public_origin)
                }
                None => return Ok(Delivery::Dropped),
            }
        }
        KIND_SIGNUP_EXISTING => render_signup_existing(public_origin),
        _ => return unknown(mail.id),
    };
    transport.send(&mail.to_email, &rendered).await?;
    Ok(Delivery::Sent)
}

/// One round: every due mail once, then the cleanup of old rows. `signups` issues the tokens of
/// verification mails; without it they cannot be sent.
pub async fn run_outbox(
    db: &PgPool,
    transport: &dyn MailTransport,
    public_origin: &str,
    signups: Option<&dyn SignupTokens>,
    config: &OutboxConfig,
) -> Result<OutboxReport, office_db::Error> {
    let mut report = OutboxReport::default();
    // A mail rescheduled during this round waits for the next one, however short its backoff.
    let round_start = time::OffsetDateTime::now_utc();
    loop {
        let due = mail_outbox::claim_due(db, round_start, config.batch, secs(config.lease)).await?;
        if due.is_empty() {
            break;
        }
        for mail in &due {
            let outcome = deliver(mail, transport, public_origin, signups).await;
            let age = time::OffsetDateTime::now_utc() - mail.created_at;
            let expired = age >= config.give_up_after;
            match outcome {
                Ok(Delivery::Sent) => {
                    mail_outbox::mark_sent(db, mail.id).await?;
                    report.sent += 1;
                }
                Ok(Delivery::Dropped) => {
                    mail_outbox::mark_failed(db, mail.id).await?;
                    report.dropped += 1;
                }
                Err(SendError::Transient) if !expired => {
                    let delay = backoff(config, mail.attempts);
                    mail_outbox::retry_in(db, mail.id, secs(delay)).await?;
                    report.retried += 1;
                }
                Err(_) => {
                    warn!(mail_id = %mail.id, attempts = mail.attempts, "mail given up");
                    mail_outbox::mark_failed(db, mail.id).await?;
                    report.failed += 1;
                }
            }
        }
    }
    report.deleted = mail_outbox::delete_done_older_than(db, secs(config.keep_done)).await?;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_up_to_the_cap() {
        let c = OutboxConfig::default();
        assert_eq!(backoff(&c, 1), Duration::from_secs(60));
        assert_eq!(backoff(&c, 2), Duration::from_secs(120));
        assert_eq!(backoff(&c, 4), Duration::from_secs(480));
        assert_eq!(backoff(&c, 50), Duration::from_secs(3600));
        assert_eq!(backoff(&c, 0), Duration::from_secs(60));
    }
}
