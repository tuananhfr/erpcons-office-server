use std::time::Duration;

use office_db::audit;

use crate::{DocError, Documents};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AuditRetentionConfig {
    /// Audit rows older than this are deleted (default 180 days).
    pub retention: Duration,
    /// Rows deleted per statement, so one run never holds a long lock.
    pub batch: i64,
}

impl Default for AuditRetentionConfig {
    fn default() -> Self {
        Self {
            retention: Duration::from_secs(180 * 24 * 3600),
            batch: 10_000,
        }
    }
}

impl Documents {
    /// Deletes audit rows past retention; returns how many. Idempotent.
    pub async fn expire_audit(&self, config: &AuditRetentionConfig) -> Result<u64, DocError> {
        let retention = i64::try_from(config.retention.as_secs()).unwrap_or(i64::MAX);
        let batch = config.batch.max(1);
        let mut deleted = 0;
        loop {
            let n = audit::delete_older_than(&self.db, retention, batch)
                .await
                .map_err(DocError::db)?;
            deleted += n;
            if n < batch as u64 {
                return Ok(deleted);
            }
        }
    }
}
