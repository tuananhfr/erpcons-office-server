//! The purge, orphan, audit retention, mail outbox and sign-up jobs beside the HTTP server.
use std::{future::Future, sync::Arc, time::Duration};

use office_db::PgPool;
use office_documents::{
    Documents,
    jobs::{AuditRetentionConfig, OrphanConfig, PurgeConfig},
};
use office_mail::{MailTransport, OutboxConfig, SignupTokens, run_outbox};
use office_signup::{PiRegistrar, PushConfig, Signups, expire, run_push};
use tokio::{
    task::JoinHandle,
    time::{Instant, MissedTickBehavior, interval_at, timeout},
};
use tracing::{error, info};

/// Old audit rows are few per run; hourly keeps each run small.
const AUDIT_EVERY: Duration = Duration::from_secs(3600);

/// "Shared with you" should arrive within a minute; an empty outbox costs one indexed query.
const MAIL_EVERY: Duration = Duration::from_secs(20);

/// Verified sign-ups wait at most this long for PI when nothing wakes the job.
const SIGNUP_PUSH_EVERY: Duration = Duration::from_secs(30);
/// Unverified sign-ups live 7 days; hourly is close enough.
const SIGNUP_EXPIRY_EVERY: Duration = Duration::from_secs(3600);

/// The outbox sender, when SMTP is configured.
pub(crate) struct MailJob {
    pub db: PgPool,
    pub transport: Arc<dyn MailTransport>,
    pub public_origin: String,
    /// Issues the tokens of verification mails.
    pub signups: Arc<Signups>,
}

/// Hands verified sign-ups to PI, when sign-up is open.
pub(crate) struct SignupJob {
    pub signups: Arc<Signups>,
    pub registrar: Arc<dyn PiRegistrar>,
}

/// `None` interval (or audit config): the job does not run.
pub(crate) struct JobsConfig {
    pub purge_every: Option<Duration>,
    pub purge: PurgeConfig,
    pub orphans_every: Option<Duration>,
    pub orphans: OrphanConfig,
    pub audit: Option<AuditRetentionConfig>,
}

/// Running jobs; stopped before the database pool closes.
pub(crate) struct Jobs(Vec<JoinHandle<()>>);

impl Jobs {
    pub(crate) fn spawn(
        docs: &Arc<Documents>,
        signups: &Arc<Signups>,
        config: &JobsConfig,
        mail: Option<MailJob>,
        signup: Option<SignupJob>,
    ) -> Self {
        let mut tasks = Vec::new();
        if let Some(mail) = mail {
            let mail = Arc::new(mail);
            tasks.push(repeat(MAIL_EVERY, move || {
                let mail = mail.clone();
                async move {
                    let config = OutboxConfig::default();
                    let tokens: &dyn SignupTokens = &*mail.signups;
                    let round = run_outbox(
                        &mail.db,
                        &*mail.transport,
                        &mail.public_origin,
                        Some(tokens),
                        &config,
                    );
                    match round.await {
                        Ok(r) if r == office_mail::OutboxReport::default() => {}
                        Ok(r) => info!(
                            sent = r.sent,
                            retried = r.retried,
                            failed = r.failed,
                            dropped = r.dropped,
                            deleted = r.deleted,
                            "mail outbox done"
                        ),
                        Err(e) => error!(error = %e, "mail outbox failed"),
                    }
                }
            }));
        }
        if let Some(job) = signup {
            tasks.push(push_signups(job));
        }
        let expiring = signups.clone();
        tasks.push(repeat(SIGNUP_EXPIRY_EVERY, move || {
            let signups = expiring.clone();
            async move {
                match expire(signups.db()).await {
                    Ok(r) if r == office_signup::ExpiryReport::default() => {}
                    Ok(r) => info!(
                        unverified = r.unverified,
                        finished = r.finished,
                        "sign-up expiry done"
                    ),
                    Err(e) => error!(error = %e, "sign-up expiry failed"),
                }
            }
        }));
        if let Some(every) = config.purge_every {
            let (docs, purge) = (docs.clone(), config.purge);
            tasks.push(repeat(every, move || {
                let docs = docs.clone();
                async move {
                    match docs.purge(&purge).await {
                        Ok(r) => info!(
                            marked = r.marked,
                            documents_purged = r.documents_purged,
                            objects_deleted = r.objects_deleted,
                            folders_deleted = r.folders_deleted,
                            shares_removed = r.shares_removed,
                            idempotency_keys_expired = r.idempotency_keys_expired,
                            "purge done"
                        ),
                        Err(e) => error!(error = %e, "purge failed"),
                    }
                }
            }));
        }
        if let Some(every) = config.orphans_every {
            let (docs, orphans) = (docs.clone(), config.orphans);
            tasks.push(repeat(every, move || {
                let docs = docs.clone();
                async move {
                    match docs.sweep_orphans(&orphans).await {
                        Ok(r) => info!(
                            unreferenced = r.unreferenced,
                            deleted = r.deleted,
                            "orphan sweep done"
                        ),
                        Err(e) => error!(error = %e, "orphan sweep failed"),
                    }
                }
            }));
        }
        if let Some(audit) = config.audit {
            let docs = docs.clone();
            tasks.push(repeat(AUDIT_EVERY, move || {
                let docs = docs.clone();
                async move {
                    match docs.expire_audit(&audit).await {
                        Ok(deleted) => info!(deleted, "audit retention done"),
                        Err(e) => error!(error = %e, "audit retention failed"),
                    }
                }
            }));
        }
        Self(tasks)
    }

    /// A run cut short is safe: the jobs are idempotent and re-check under locks; a mail cut short
    /// mid-send is sent again once its lease ends.
    pub(crate) fn stop(self) {
        for task in self.0 {
            task.abort();
        }
    }
}

/// Rounds of the PI job: right after a verification, else every [`SIGNUP_PUSH_EVERY`].
fn push_signups(job: SignupJob) -> JoinHandle<()> {
    tokio::spawn(async move {
        let verified = job.signups.verified_signal();
        let config = PushConfig::default();
        loop {
            let _ = timeout(SIGNUP_PUSH_EVERY, verified.notified()).await;
            match run_push(job.signups.db(), &*job.registrar, &config).await {
                Ok(r) if r == office_signup::PushReport::default() => {}
                Ok(r) => info!(
                    created = r.created,
                    existing = r.existing,
                    retried = r.retried,
                    failed = r.failed,
                    "sign-ups sent to PI"
                ),
                Err(e) => error!(error = %e, "sign-up job failed"),
            }
        }
    })
}

/// Runs `job` every `every`, first after one interval so a restart loop does not hammer the
/// database; a slow run delays the next one instead of piling up.
fn repeat<F, Fut>(every: Duration, job: F) -> JoinHandle<()>
where
    F: Fn() -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send,
{
    tokio::spawn(async move {
        let mut ticks = interval_at(Instant::now() + every, every);
        ticks.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            ticks.tick().await;
            job().await;
        }
    })
}
