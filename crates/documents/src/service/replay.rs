//! A save the server already recorded, sent again because its response was lost (E2E T07c).
use office_db::versions;
use office_storage::{ObjectReader, StorageError, digest_stream};

use super::{Caller, Documents, access::Accessed, content::version_etag};
use crate::{DocError, Document};

impl Documents {
    /// The repeat of the save that made `current`: `if_match` names the version just before it, the
    /// caller wrote it, and the bytes are the same. Then the answer is `current`, with no new
    /// version and no quota; anything else is the usual conflict.
    pub(super) async fn replay_or_conflict(
        &self,
        c: &Caller<'_>,
        current: Accessed,
        if_match: &[String],
        source: ObjectReader,
    ) -> Result<Document, DocError> {
        let conflict = DocError::VersionConflict {
            latest_version: current.row.version,
        };
        let base = versions::replay_base(&self.db, current.row.id)
            .await
            .map_err(DocError::db)?;
        let Some(base) = base else {
            return Err(conflict);
        };
        // Checked before reading the body, so an ordinary stale save is refused without it.
        if base.author_id != c.actor_id() || !if_match.contains(&version_etag(base.previous_id)) {
            return Err(conflict);
        }
        match digest_stream(source, self.quota.max_file_bytes).await {
            Ok(digest) if digest.to_hex() == base.sha256 => current.document(),
            Ok(_) | Err(StorageError::TooLarge(_)) => Err(conflict),
            Err(e) => Err(DocError::storage(e)),
        }
    }
}
