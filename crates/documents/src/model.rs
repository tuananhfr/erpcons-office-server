use office_db::{ItemState, documents::DocumentRow, folders::FolderRow, versions::VersionRow};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{DocError, Format};

/// A folder as the service returns it. No `Debug`: the name is personal data.
#[derive(Clone)]
pub struct Folder {
    pub id: Uuid,
    pub parent_id: Option<Uuid>,
    pub name: String,
    pub state: ItemState,
    pub trashed_at: Option<OffsetDateTime>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

impl From<FolderRow> for Folder {
    fn from(r: FolderRow) -> Self {
        Self {
            id: r.id,
            parent_id: r.parent_id,
            name: r.name,
            state: r.state,
            trashed_at: r.trashed_at,
            created_at: r.created_at,
            updated_at: r.updated_at,
        }
    }
}

/// A document with its current version. No `Debug`: the title is personal data.
#[derive(Clone)]
pub struct Document {
    pub id: Uuid,
    pub folder_id: Option<Uuid>,
    pub title: String,
    pub format: Format,
    pub size: u64,
    pub version: i32,
    pub etag: String,
    pub state: ItemState,
    pub trashed_at: Option<OffsetDateTime>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

impl TryFrom<DocumentRow> for Document {
    type Error = DocError;

    fn try_from(r: DocumentRow) -> Result<Self, DocError> {
        let format = Format::from_mime(&r.mime)
            .filter(|f| f.kind().as_str() == r.kind)
            .ok_or_else(|| {
                tracing::error!(document_id = %r.id, "document has an unknown format");
                DocError::Unavailable
            })?;
        Ok(Self {
            id: r.id,
            folder_id: r.folder_id,
            title: r.title,
            format,
            size: u64::try_from(r.size).unwrap_or(0),
            version: r.version,
            etag: r.etag,
            state: r.state,
            trashed_at: r.trashed_at,
            created_at: r.created_at,
            updated_at: r.updated_at,
        })
    }
}

/// One immutable version. No `Debug`: the author name is personal data.
#[derive(Clone)]
pub struct Version {
    pub version: i32,
    pub size: u64,
    pub sha256: String,
    pub author_name: String,
    pub created_at: OffsetDateTime,
}

impl From<VersionRow> for Version {
    fn from(r: VersionRow) -> Self {
        Self {
            version: r.seq,
            size: u64::try_from(r.bytes).unwrap_or(0),
            sha256: r.sha256,
            author_name: r.author_name_snapshot,
            created_at: r.created_at,
        }
    }
}

/// One page of a listing; `next_cursor` is `None` on the last page.
pub struct Page<T> {
    pub items: T,
    pub next_cursor: Option<String>,
}

/// Folders and documents of a folder listing or of the trash.
pub struct Entries {
    pub folders: Vec<Folder>,
    pub documents: Vec<Document>,
}

/// Requested page: `limit` within 1–200 (checked by the HTTP layer), opaque `cursor`.
#[derive(Clone, Debug)]
pub struct PageRequest {
    pub cursor: Option<String>,
    pub limit: u32,
}
