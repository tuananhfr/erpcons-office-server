use std::{
    collections::HashSet,
    time::{Duration, SystemTime},
};

use office_db::{locks, objects};
use office_storage::ObjectKey;

use crate::{DocError, Documents};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OrphanConfig {
    /// Younger objects may belong to a save whose transaction has not committed yet.
    pub min_age: Duration,
    /// Keys listed per storage call.
    pub page: usize,
}

impl Default for OrphanConfig {
    fn default() -> Self {
        Self {
            min_age: Duration::from_secs(3600),
            page: 500,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OrphanReport {
    pub unreferenced: u64,
    pub deleted: u64,
}

impl Documents {
    /// Deletes stored objects no `storage_objects` row keeps (a save that failed after `put`)
    /// once they are older than `min_age`. Idempotent; safe while saves run.
    pub async fn sweep_orphans(&self, config: &OrphanConfig) -> Result<OrphanReport, DocError> {
        let mut report = OrphanReport::default();
        let mut after: Option<ObjectKey> = None;
        loop {
            let keys = self
                .storage
                .list(after.as_ref(), config.page.max(1))
                .await
                .map_err(DocError::storage)?;
            let Some(last) = keys.last().cloned() else {
                break;
            };
            let names: Vec<String> = keys.iter().map(|k| k.as_str().to_owned()).collect();
            let kept: HashSet<String> = objects::kept_keys(&self.db, &names)
                .await
                .map_err(DocError::db)?
                .into_iter()
                .collect();
            for key in keys.iter().filter(|k| !kept.contains(k.as_str())) {
                report.unreferenced += 1;
                if self.delete_orphan(key, config.min_age).await? {
                    report.deleted += 1;
                }
            }
            if keys.len() < config.page.max(1) {
                break;
            }
            after = Some(last);
        }
        Ok(report)
    }

    /// Re-checks under the object's lock, which a save holds from before it records the object
    /// until its commit; a save that finds the object gone stores it again.
    async fn delete_orphan(&self, key: &ObjectKey, min_age: Duration) -> Result<bool, DocError> {
        let mut tx = self.begin().await?;
        // Filesystem keys are the sha256, the same string saves lock on.
        locks::object(&mut tx, key.as_str())
            .await
            .map_err(DocError::db)?;
        let kept = objects::kept_keys(&mut *tx, &[key.as_str().to_owned()])
            .await
            .map_err(DocError::db)?;
        if !kept.is_empty() {
            return Ok(false);
        }
        let Some(meta) = self.storage.head(key).await.map_err(DocError::storage)? else {
            return Ok(false);
        };
        let age = meta
            .modified
            .map(|m| SystemTime::now().duration_since(m).unwrap_or_default());
        if age.is_none_or(|a| a < min_age) {
            return Ok(false);
        }
        let deleted = self.storage.delete(key).await.map_err(DocError::storage)?;
        tx.commit().await.map_err(DocError::db)?;
        Ok(deleted)
    }
}
