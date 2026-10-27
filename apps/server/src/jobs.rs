//! The purge and orphan jobs on fixed intervals, beside the HTTP server.
use std::{future::Future, sync::Arc, time::Duration};

use office_documents::{
    Documents,
    jobs::{OrphanConfig, PurgeConfig},
};
use tokio::{
    task::JoinHandle,
    time::{Instant, MissedTickBehavior, interval_at},
};
use tracing::{error, info};

/// `None` interval: the job does not run.
pub(crate) struct JobsConfig {
    pub purge_every: Option<Duration>,
    pub purge: PurgeConfig,
    pub orphans_every: Option<Duration>,
    pub orphans: OrphanConfig,
}

/// Running jobs; stopped before the database pool closes.
pub(crate) struct Jobs(Vec<JoinHandle<()>>);

impl Jobs {
    pub(crate) fn spawn(docs: &Arc<Documents>, config: &JobsConfig) -> Self {
        let mut tasks = Vec::new();
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
        Self(tasks)
    }

    /// A run cut short is safe: both jobs are idempotent and re-check under locks.
    pub(crate) fn stop(self) {
        for task in self.0 {
            task.abort();
        }
    }
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
