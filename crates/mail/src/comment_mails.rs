//! The document service's say on a comment mail once it is due: people who lost access get none.
use async_trait::async_trait;

use crate::{CommentNotice, SendError};

#[async_trait]
pub trait CommentMails: Send + Sync {
    /// `None` drops the mail: its recipient may no longer read the document's comments, or there is
    /// nothing left to tell. Otherwise, for new threads, how many there are now (0 also drops).
    /// `Err` tries again later.
    async fn still_due(&self, kind: &str, notice: &CommentNotice)
    -> Result<Option<u32>, SendError>;
}
