//! Response bodies of the comment operations (`CommentThread`, `Comment`).
use office_documents::{Comment, Thread, Threads};
use serde::Serialize;
use serde_json::Value;
use time::OffsetDateTime;
use uuid::Uuid;

#[derive(Serialize)]
pub(crate) struct CommentDto<'a> {
    id: Uuid,
    thread_id: Uuid,
    author_name: &'a str,
    mine: bool,
    body: Option<&'a str>,
    #[serde(serialize_with = "time::serde::rfc3339::serialize")]
    created_at: OffsetDateTime,
    #[serde(serialize_with = "time::serde::rfc3339::option::serialize")]
    edited_at: Option<OffsetDateTime>,
    #[serde(serialize_with = "time::serde::rfc3339::option::serialize")]
    deleted_at: Option<OffsetDateTime>,
}

impl<'a> From<&'a Comment> for CommentDto<'a> {
    fn from(c: &'a Comment) -> Self {
        Self {
            id: c.id,
            thread_id: c.thread_id,
            author_name: &c.author_name,
            mine: c.mine,
            body: c.body.as_deref(),
            created_at: c.created_at,
            edited_at: c.edited_at,
            deleted_at: c.deleted_at,
        }
    }
}

#[derive(Serialize)]
pub(crate) struct ThreadDto<'a> {
    id: Uuid,
    document_id: Uuid,
    /// Stored as the editor sent it; the server never reads inside.
    anchor: Value,
    region: Option<&'a str>,
    created_version: i32,
    status: &'static str,
    created_by_name: &'a str,
    mine: bool,
    #[serde(serialize_with = "time::serde::rfc3339::serialize")]
    created_at: OffsetDateTime,
    #[serde(serialize_with = "time::serde::rfc3339::serialize")]
    updated_at: OffsetDateTime,
    resolved_by_name: Option<&'a str>,
    #[serde(serialize_with = "time::serde::rfc3339::option::serialize")]
    resolved_at: Option<OffsetDateTime>,
    comments: Vec<CommentDto<'a>>,
}

impl<'a> From<&'a Thread> for ThreadDto<'a> {
    fn from(t: &'a Thread) -> Self {
        Self {
            id: t.id,
            document_id: t.document_id,
            anchor: serde_json::from_str(&t.anchor).unwrap_or(Value::Null),
            region: t.region.as_deref(),
            created_version: t.created_version,
            status: t.status.as_str(),
            created_by_name: &t.created_by_name,
            mine: t.mine,
            created_at: t.created_at,
            updated_at: t.updated_at,
            resolved_by_name: t.resolved_by_name.as_deref(),
            resolved_at: t.resolved_at,
            comments: t.comments.iter().map(CommentDto::from).collect(),
        }
    }
}

#[derive(Serialize)]
pub(crate) struct ThreadsDto<'a> {
    threads: Vec<ThreadDto<'a>>,
    next_cursor: Option<&'a str>,
    can_resolve: bool,
}

impl<'a> From<&'a Threads> for ThreadsDto<'a> {
    fn from(t: &'a Threads) -> Self {
        Self {
            threads: t.threads.iter().map(ThreadDto::from).collect(),
            next_cursor: t.next_cursor.as_deref(),
            can_resolve: t.can_resolve,
        }
    }
}

#[derive(Serialize)]
pub(crate) struct AnchorsUpdatedDto {
    pub updated: u32,
}
