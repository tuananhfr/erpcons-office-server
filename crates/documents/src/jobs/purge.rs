use std::time::Duration;

use office_db::{idempotency, locks, purge};
use office_storage::ObjectKey;
use uuid::Uuid;

use crate::{
    DocError, Documents,
    service::{Subject, audit_system},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PurgeConfig {
    /// How long trashed items stay restorable (plan §10: 30 days).
    pub retention: Duration,
    /// Objects examined per query.
    pub batch: i64,
}

impl Default for PurgeConfig {
    fn default() -> Self {
        Self {
            retention: Duration::from_secs(30 * 24 * 3600),
            batch: 100,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PurgeReport {
    pub marked: u64,
    pub objects_deleted: u64,
    pub documents_purged: u64,
    pub folders_deleted: u64,
    /// Permissions and share settings of purged documents.
    pub shares_removed: u64,
    pub idempotency_keys_expired: u64,
}

impl Documents {
    /// Trashed documents past retention become `PURGE_PENDING`, their bytes no live document
    /// shares are deleted, then they become `PURGED` (metadata kept, bytes gone). Expired trashed
    /// folders are deleted. Idempotent; safe while saves run, as every deletion re-checks under
    /// the object's lock that nothing live has started using it.
    pub async fn purge(&self, config: &PurgeConfig) -> Result<PurgeReport, DocError> {
        let trace_id = Uuid::now_v7().to_string();
        let retention = i64::try_from(config.retention.as_secs()).unwrap_or(i64::MAX);
        let mut report = PurgeReport {
            marked: purge::mark_expired(&self.db, retention)
                .await
                .map_err(DocError::db)?,
            ..PurgeReport::default()
        };
        loop {
            let dead = purge::dead_objects(&self.db, config.batch)
                .await
                .map_err(DocError::db)?;
            if dead.is_empty() {
                break;
            }
            let mut progressed = false;
            for object in &dead {
                if self.delete_dead_object(object).await? {
                    report.objects_deleted += 1;
                    progressed = true;
                }
            }
            if !progressed {
                break;
            }
        }
        let mut tx = self.begin().await?;
        let purged = purge::finish(&mut tx).await.map_err(DocError::db)?;
        for id in &purged {
            audit_system(&mut tx, "document.purge", Subject::Document(*id), &trace_id).await?;
        }
        report.documents_purged = purged.len() as u64;
        // Also catches what a purge by an older binary, which knew no sharing, left behind.
        report.shares_removed = purge::forget_sharing(&mut tx, None)
            .await
            .map_err(DocError::db)?;
        loop {
            let deleted = purge::delete_expired_folders(&mut tx, retention)
                .await
                .map_err(DocError::db)?;
            for id in &deleted {
                audit_system(&mut tx, "folder.purge", Subject::Folder(*id), &trace_id).await?;
            }
            report.folders_deleted += deleted.len() as u64;
            if deleted.is_empty() {
                break;
            }
        }
        tx.commit().await.map_err(DocError::db)?;
        report.idempotency_keys_expired = idempotency::delete_expired(&self.db)
            .await
            .map_err(DocError::db)?;
        Ok(report)
    }

    /// Deletes `object`'s bytes when, under its lock, nothing live uses it.
    pub(crate) async fn delete_dead_object(
        &self,
        object: &purge::DeadObject,
    ) -> Result<bool, DocError> {
        let mut tx = self.begin().await?;
        locks::object(&mut tx, &object.sha256)
            .await
            .map_err(DocError::db)?;
        if !purge::still_dead(&mut tx, object.id)
            .await
            .map_err(DocError::db)?
        {
            return Ok(false);
        }
        self.storage
            .delete(&ObjectKey::new(object.opaque_key.clone()))
            .await
            .map_err(DocError::storage)?;
        purge::mark_deleted(&mut *tx, object.id)
            .await
            .map_err(DocError::db)?;
        tx.commit().await.map_err(DocError::db)?;
        Ok(true)
    }
}
