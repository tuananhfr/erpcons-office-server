//! Bytes stored ahead of the transaction that records them (plan §4), and their removal when that
//! transaction never comes.
use std::io::Cursor;

use office_db::{locks, objects, quota};
use office_storage::{ObjectKey, ObjectReader, PutOptions, Sha256Digest};
use tracing::error;

use super::{Caller, Documents, audit::Subject};
use crate::{DocError, FailPoint, Format, intake::ValidatingReader, templates};

/// Bytes accepted and stored, not yet part of any document.
#[derive(Clone, Debug)]
pub struct Staged {
    pub key: ObjectKey,
    pub sha256: Sha256Digest,
    pub size: u64,
    pub format: Format,
    /// The bytes were already stored: someone else's row may be about to use them.
    pub deduplicated: bool,
}

impl Documents {
    /// Streams an upload to storage, checking signature, ZIP structure and the per-file limit on
    /// the way. Nothing is kept when a check fails.
    pub async fn stage_upload(
        &self,
        c: &Caller<'_>,
        format: Format,
        source: ObjectReader,
    ) -> Result<Staged, DocError> {
        let result = match self.check_room(c.actor_id()).await {
            Ok(()) => self.stage(format, source).await,
            Err(e) => Err(e),
        };
        self.audit_failure(c, "document.create", Subject::None, &result)
            .await;
        result
    }

    /// Stores the blank template of `format`.
    pub async fn stage_blank(&self, c: &Caller<'_>, format: Format) -> Result<Staged, DocError> {
        let result = match self.check_room(c.actor_id()).await {
            Ok(()) => {
                self.stage(format, Box::pin(Cursor::new(templates::blank(format))))
                    .await
            }
            Err(e) => Err(e),
        };
        self.audit_failure(c, "document.create", Subject::None, &result)
            .await;
        result
    }

    /// Refuses an owner already at its quota before any byte is stored. The exact check, with the
    /// incoming size, runs again in the transaction that records the bytes.
    pub(super) async fn check_room(&self, owner_id: &str) -> Result<(), DocError> {
        let used = quota::used_bytes(&self.db, owner_id)
            .await
            .map_err(DocError::db)?;
        if u64::try_from(used).unwrap_or(u64::MAX) >= self.quota.limit_bytes {
            return Err(DocError::QuotaExceeded);
        }
        Ok(())
    }

    pub(super) async fn stage(
        &self,
        format: Format,
        source: ObjectReader,
    ) -> Result<Staged, DocError> {
        let reader = ValidatingReader::new(source, format, self.zip_limits);
        let options = PutOptions {
            max_bytes: Some(self.quota.max_file_bytes),
            ..PutOptions::default()
        };
        let put = self
            .storage
            .put(Box::pin(reader), options)
            .await
            .map_err(DocError::storage)?;
        let staged = Staged {
            key: put.key,
            sha256: put.sha256,
            size: put.size,
            format,
            deduplicated: put.deduplicated,
        };
        self.fail_points.pause(FailPoint::AfterPut).await;
        if self.fail_points.fires(FailPoint::AfterPut) {
            self.discard(&staged).await;
            return Err(DocError::Unavailable);
        }
        Ok(staged)
    }

    /// Removes bytes a failed request stored, unless a row keeps them or they were already there
    /// (another request may have deduplicated onto them). Best effort: the orphan sweep is the
    /// backstop, so a failure here is logged, not returned.
    pub async fn discard(&self, staged: &Staged) {
        if staged.deduplicated {
            return;
        }
        if let Err(e) = self.try_discard(staged).await {
            error!(error = %e, "discarding staged bytes failed");
        }
    }

    async fn try_discard(&self, staged: &Staged) -> Result<(), DocError> {
        let mut tx = self.begin().await?;
        // Saves hold this lock from before they record an object until their commit.
        locks::object(&mut tx, &staged.sha256.to_hex())
            .await
            .map_err(DocError::db)?;
        let kept = objects::kept_keys(&mut *tx, &[staged.key.as_str().to_owned()])
            .await
            .map_err(DocError::db)?;
        if kept.is_empty() {
            self.storage
                .delete(&staged.key)
                .await
                .map_err(DocError::storage)?;
        }
        tx.commit().await.map_err(DocError::db)
    }
}
