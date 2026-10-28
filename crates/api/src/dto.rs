//! Response bodies of the contract (`Folder`, `Document`, `Version` and the listings).
use office_documents::{Document, Entries, Folder, FolderPath, Page, Version};
use serde::Serialize;
use time::OffsetDateTime;
use uuid::Uuid;

#[derive(Serialize)]
pub(crate) struct FolderDto<'a> {
    id: Uuid,
    parent_id: Option<Uuid>,
    name: &'a str,
    state: &'static str,
    #[serde(serialize_with = "time::serde::rfc3339::option::serialize")]
    trashed_at: Option<OffsetDateTime>,
    #[serde(serialize_with = "time::serde::rfc3339::serialize")]
    created_at: OffsetDateTime,
    #[serde(serialize_with = "time::serde::rfc3339::serialize")]
    updated_at: OffsetDateTime,
}

impl<'a> From<&'a Folder> for FolderDto<'a> {
    fn from(f: &'a Folder) -> Self {
        Self {
            id: f.id,
            parent_id: f.parent_id,
            name: &f.name,
            state: f.state.as_str(),
            trashed_at: f.trashed_at,
            created_at: f.created_at,
            updated_at: f.updated_at,
        }
    }
}

/// `getFolder`: the folder and the folders above it, top level first.
#[derive(Serialize)]
pub(crate) struct FolderPathDto<'a> {
    folder: FolderDto<'a>,
    ancestors: Vec<FolderDto<'a>>,
}

impl<'a> From<&'a FolderPath> for FolderPathDto<'a> {
    fn from(p: &'a FolderPath) -> Self {
        Self {
            folder: FolderDto::from(&p.folder),
            ancestors: p.ancestors.iter().map(FolderDto::from).collect(),
        }
    }
}

#[derive(Serialize)]
pub(crate) struct DocumentDto<'a> {
    id: Uuid,
    folder_id: Option<Uuid>,
    title: &'a str,
    kind: &'static str,
    format: &'static str,
    size: u64,
    version: i32,
    /// Phase 1 returns only durable versions (§11).
    save_state: &'static str,
    state: &'static str,
    #[serde(serialize_with = "time::serde::rfc3339::option::serialize")]
    trashed_at: Option<OffsetDateTime>,
    #[serde(serialize_with = "time::serde::rfc3339::serialize")]
    created_at: OffsetDateTime,
    #[serde(serialize_with = "time::serde::rfc3339::serialize")]
    updated_at: OffsetDateTime,
}

impl<'a> From<&'a Document> for DocumentDto<'a> {
    fn from(d: &'a Document) -> Self {
        Self {
            id: d.id,
            folder_id: d.folder_id,
            title: &d.title,
            kind: d.format.kind().as_str(),
            format: d.format.extension(),
            size: d.size,
            version: d.version,
            save_state: "SERVER_DURABLE",
            state: d.state.as_str(),
            trashed_at: d.trashed_at,
            created_at: d.created_at,
            updated_at: d.updated_at,
        }
    }
}

#[derive(Serialize)]
pub(crate) struct VersionDto<'a> {
    version: i32,
    size: u64,
    sha256: &'a str,
    author_name: &'a str,
    #[serde(serialize_with = "time::serde::rfc3339::serialize")]
    created_at: OffsetDateTime,
}

impl<'a> From<&'a Version> for VersionDto<'a> {
    fn from(v: &'a Version) -> Self {
        Self {
            version: v.version,
            size: v.size,
            sha256: &v.sha256,
            author_name: &v.author_name,
            created_at: v.created_at,
        }
    }
}

/// `listChildren` and `listTrash`.
#[derive(Serialize)]
pub(crate) struct EntriesDto<'a> {
    folders: Vec<FolderDto<'a>>,
    documents: Vec<DocumentDto<'a>>,
    next_cursor: Option<&'a str>,
}

impl<'a> From<&'a Page<Entries>> for EntriesDto<'a> {
    fn from(p: &'a Page<Entries>) -> Self {
        Self {
            folders: p.items.folders.iter().map(FolderDto::from).collect(),
            documents: p.items.documents.iter().map(DocumentDto::from).collect(),
            next_cursor: p.next_cursor.as_deref(),
        }
    }
}

#[derive(Serialize)]
pub(crate) struct VersionsDto<'a> {
    versions: Vec<VersionDto<'a>>,
    next_cursor: Option<&'a str>,
}

impl<'a> From<&'a Page<Vec<Version>>> for VersionsDto<'a> {
    fn from(p: &'a Page<Vec<Version>>) -> Self {
        Self {
            versions: p.items.iter().map(VersionDto::from).collect(),
            next_cursor: p.next_cursor.as_deref(),
        }
    }
}
