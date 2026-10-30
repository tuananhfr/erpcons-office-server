use office_db::{PgPool, signups};

use crate::UNVERIFIED_RETENTION;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ExpiryReport {
    /// Never verified within 7 days of their last submission.
    pub unverified: u64,
    /// Registered or given up more than 7 days ago; they still held an address and a name.
    pub finished: u64,
}

/// Deletes sign-ups nobody verified in time, and finished ones once old.
pub async fn expire(db: &PgPool) -> Result<ExpiryReport, office_db::Error> {
    let secs = i64::try_from(UNVERIFIED_RETENTION.as_secs()).unwrap_or(i64::MAX);
    let (unverified, finished) = signups::expire(db, secs, secs).await?;
    Ok(ExpiryReport {
        unverified,
        finished,
    })
}
