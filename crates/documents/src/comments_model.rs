//! What the comment operations take and return (plan-m5). No `Debug` on anything holding words,
//! names or anchors: they are personal data.
use office_db::comments::{CommentRow, ThreadRow};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::DocError;

/// Longest comment, in characters.
pub const MAX_BODY_CHARS: usize = 10_000;
/// Largest anchor, as compact JSON.
pub const MAX_ANCHOR_BYTES: usize = 8 * 1024;
/// Longest region id, in bytes (the live channel's rule).
pub const MAX_REGION_BYTES: usize = 256;
/// Threads one document may hold, open, resolved and orphaned together.
pub const MAX_THREADS_PER_DOCUMENT: i64 = 2_000;
/// Comments one thread may hold, the first and deleted ones included.
pub const MAX_COMMENTS_PER_THREAD: i64 = 200;
/// Threads one anchor batch may re-place.
pub const MAX_ANCHOR_CHANGES: usize = 200;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThreadStatus {
    Open,
    Resolved,
    /// The editor could not find its anchor again.
    Orphaned,
}

impl ThreadStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Resolved => "resolved",
            Self::Orphaned => "orphaned",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "open" => Some(Self::Open),
            "resolved" => Some(Self::Resolved),
            "orphaned" => Some(Self::Orphaned),
            _ => None,
        }
    }
}

/// A comment's words: 1–10 000 characters, not only blanks, no NUL (Postgres refuses it).
#[derive(Clone, PartialEq, Eq)]
pub struct Body(String);

impl Body {
    pub fn parse(raw: String) -> Result<Self, DocError> {
        let ok =
            !raw.trim().is_empty() && raw.chars().count() <= MAX_BODY_CHARS && !raw.contains('\0');
        if ok {
            Ok(Self(raw))
        } else {
            Err(DocError::InvalidRequest)
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// An anchor as compact JSON text of an object, at most [`MAX_ANCHOR_BYTES`].
#[derive(Clone, PartialEq, Eq)]
pub struct Anchor(String);

impl Anchor {
    pub fn parse(json_object: String) -> Result<Self, DocError> {
        if json_object.starts_with('{')
            && json_object.len() <= MAX_ANCHOR_BYTES
            && !json_object.contains("\\u0000")
        {
            Ok(Self(json_object))
        } else {
            Err(DocError::InvalidRequest)
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A stable region id: 1–256 bytes without control characters.
pub fn check_region(region: &str) -> Result<(), DocError> {
    let ok = !region.is_empty()
        && region.len() <= MAX_REGION_BYTES
        && !region.chars().any(char::is_control);
    if ok {
        Ok(())
    } else {
        Err(DocError::InvalidRequest)
    }
}

/// A thread to start: where, and its first comment.
pub struct NewThread {
    pub anchor: Anchor,
    pub region: Option<String>,
    /// The version the anchor was taken on; the current one when left out.
    pub version: Option<i32>,
    pub body: Body,
}

/// What an editor found when it placed a thread again; `None` fields stay as they are.
pub struct AnchorChange {
    pub thread_id: Uuid,
    pub anchor: Option<Anchor>,
    /// `Some(None)` clears the region.
    pub region: Option<Option<String>>,
    /// `Open` or `Orphaned`; a resolved thread keeps its status.
    pub status: Option<ThreadStatus>,
}

#[derive(Clone)]
pub struct Comment {
    pub id: Uuid,
    pub thread_id: Uuid,
    pub author_name: String,
    /// Written by the caller: it may edit and delete it.
    pub mine: bool,
    /// `None` once deleted.
    pub body: Option<String>,
    pub created_at: OffsetDateTime,
    pub edited_at: Option<OffsetDateTime>,
    pub deleted_at: Option<OffsetDateTime>,
}

impl Comment {
    pub(crate) fn from_row(row: CommentRow, caller: &str) -> Self {
        Self {
            id: row.id,
            thread_id: row.thread_id,
            mine: row.author_id == caller,
            author_name: row.author_name,
            body: row.body,
            created_at: row.created_at,
            edited_at: row.edited_at,
            deleted_at: row.deleted_at,
        }
    }
}

#[derive(Clone)]
pub struct Thread {
    pub id: Uuid,
    pub document_id: Uuid,
    pub anchor: String,
    pub region: Option<String>,
    pub created_version: i32,
    pub status: ThreadStatus,
    pub created_by_name: String,
    /// Started by the caller.
    pub mine: bool,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
    pub resolved_by_name: Option<String>,
    pub resolved_at: Option<OffsetDateTime>,
    /// The first comment, then the replies, deleted ones in their place.
    pub comments: Vec<Comment>,
}

impl Thread {
    pub(crate) fn from_row(
        row: ThreadRow,
        caller: &str,
        comments: Vec<Comment>,
    ) -> Result<Self, DocError> {
        let status = ThreadStatus::parse(&row.status).ok_or_else(|| {
            tracing::error!(thread_id = %row.id, "comment thread has an unknown status");
            DocError::Unavailable
        })?;
        Ok(Self {
            id: row.id,
            document_id: row.document_id,
            mine: row.created_by == caller,
            anchor: row.anchor,
            region: row.region,
            created_version: row.created_version_seq,
            status,
            created_by_name: row.created_by_name,
            created_at: row.created_at,
            updated_at: row.updated_at,
            resolved_by_name: row.resolved_by_name,
            resolved_at: row.resolved_at,
            comments,
        })
    }
}

/// A page of threads, and what the caller may do with them.
pub struct Threads {
    pub threads: Vec<Thread>,
    pub next_cursor: Option<String>,
    /// Resolve, reopen and delete whole threads (owner and editors).
    pub can_resolve: bool,
}
