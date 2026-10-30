//! The job that hands verified sign-ups to PI: each is claimed, sent to `register`, then finished
//! (hash dropped) or retried with backoff. One PI never answered for 7 days is given up.
use std::time::Duration;

use office_db::{PgPool, mail_outbox, signups};
use office_mail::{KIND_SIGNUP_EXISTING, SignupNotice};

use crate::{PiRegistrar, RegisterError, RegisterRequest, Registered, portable_hash};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PushConfig {
    pub batch: i64,
    /// A claimed sign-up is free again after this, should the job die mid-call.
    pub lease: Duration,
    /// First retry delay; it doubles per attempt up to `max_backoff`.
    pub first_backoff: Duration,
    pub max_backoff: Duration,
    /// Verified this long ago and PI still has not answered for good: given up, hash dropped.
    pub give_up_after: Duration,
    /// Queue "this address already had a PI ID account" when PI kept the old password.
    pub mail_enabled: bool,
}

impl Default for PushConfig {
    fn default() -> Self {
        Self {
            batch: 10,
            lease: Duration::from_secs(300),
            first_backoff: Duration::from_secs(30),
            max_backoff: Duration::from_secs(3600),
            give_up_after: Duration::from_secs(7 * 24 * 3600),
            mail_enabled: true,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PushReport {
    pub created: u64,
    pub existing: u64,
    pub retried: u64,
    pub failed: u64,
}

fn secs(d: Duration) -> i64 {
    i64::try_from(d.as_secs()).unwrap_or(i64::MAX)
}

fn backoff(config: &PushConfig, attempts: i32) -> Duration {
    let doublings = u32::try_from(attempts.saturating_sub(1))
        .unwrap_or(0)
        .min(20);
    config
        .first_backoff
        .saturating_mul(1 << doublings)
        .min(config.max_backoff)
}

/// Ends the sign-up as registered and, when the person must keep using an older PI ID password,
/// tells them so by mail, in one transaction.
async fn registered(
    db: &PgPool,
    row: &signups::PushRow,
    outcome: Registered,
    config: &PushConfig,
) -> Result<(), office_db::Error> {
    let mut tx = db.begin().await?;
    if !signups::finish(&mut *tx, row.id, true).await? {
        return Ok(());
    }
    if config.mail_enabled && outcome.keeps_old_password() {
        let payload = SignupNotice { signup_id: row.id }.to_json();
        let mail = mail_outbox::NewMail {
            kind: KIND_SIGNUP_EXISTING,
            sender_id: None,
            to_email: &row.email,
            payload: &payload,
        };
        mail_outbox::enqueue(&mut tx, &mail).await?;
    }
    tx.commit().await
}

/// One round: every due sign-up once.
pub async fn run_push(
    db: &PgPool,
    registrar: &dyn PiRegistrar,
    config: &PushConfig,
) -> Result<PushReport, office_db::Error> {
    let mut report = PushReport::default();
    let round_start = time::OffsetDateTime::now_utc();
    loop {
        let due = signups::claim_push(db, round_start, config.batch, secs(config.lease)).await?;
        if due.is_empty() {
            break;
        }
        for row in &due {
            if !portable_hash(&row.password_hash) {
                tracing::error!(signup_id = %row.id, "sign-up hash PI would not take");
                signups::finish(db, row.id, false).await?;
                report.failed += 1;
                continue;
            }
            let request = RegisterRequest::new(
                row.id.to_string(),
                &row.email,
                &row.display_name,
                &row.password_hash,
            );
            let age = time::OffsetDateTime::now_utc() - row.verified_at;
            match registrar.register(&request).await {
                Ok(outcome) => {
                    if outcome
                        == (Registered::Created {
                            password_adopted: false,
                        })
                    {
                        tracing::error!(signup_id = %row.id, "PI created the account without its password");
                    }
                    registered(db, row, outcome, config).await?;
                    match outcome {
                        Registered::Created { .. } => report.created += 1,
                        _ => report.existing += 1,
                    }
                }
                Err(RegisterError::Transient) if age < config.give_up_after => {
                    signups::retry_push_in(db, row.id, secs(backoff(config, row.push_attempts)))
                        .await?;
                    report.retried += 1;
                }
                Err(e) => {
                    tracing::warn!(signup_id = %row.id, attempts = row.push_attempts, error = %e, "sign-up given up");
                    signups::finish(db, row.id, false).await?;
                    report.failed += 1;
                }
            }
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_up_to_the_cap() {
        let c = PushConfig::default();
        assert_eq!(backoff(&c, 1), Duration::from_secs(30));
        assert_eq!(backoff(&c, 3), Duration::from_secs(120));
        assert_eq!(backoff(&c, 40), Duration::from_secs(3600));
    }
}
