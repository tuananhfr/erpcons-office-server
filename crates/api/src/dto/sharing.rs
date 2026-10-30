//! Response bodies of the sharing operations.
use office_documents::{
    AddedPermissions, LinkCreated, Page, Permission, Permissions, Person, ShareSettings,
    SharedDocument, SharedList,
};
use serde::Serialize;
use time::OffsetDateTime;
use uuid::Uuid;

use super::DocumentDto;

#[derive(Serialize)]
struct PersonDto<'a> {
    actor_id: &'a str,
    display_name: &'a str,
    email: Option<&'a str>,
}

impl<'a> From<&'a Person> for PersonDto<'a> {
    fn from(p: &'a Person) -> Self {
        Self {
            actor_id: &p.actor_id,
            display_name: &p.display_name,
            email: p.email.as_deref(),
        }
    }
}

#[derive(Serialize)]
pub(crate) struct PermissionDto<'a> {
    id: Uuid,
    actor_id: Option<&'a str>,
    display_name: Option<&'a str>,
    email: Option<&'a str>,
    role: &'static str,
    pending: bool,
    #[serde(serialize_with = "time::serde::rfc3339::serialize")]
    created_at: OffsetDateTime,
    #[serde(serialize_with = "time::serde::rfc3339::option::serialize")]
    expires_at: Option<OffsetDateTime>,
}

impl<'a> From<&'a Permission> for PermissionDto<'a> {
    fn from(p: &'a Permission) -> Self {
        Self {
            id: p.id,
            actor_id: p.actor_id.as_deref(),
            display_name: p.display_name.as_deref(),
            email: p.email.as_deref(),
            role: p.role.as_str(),
            pending: p.pending(),
            created_at: p.created_at,
            expires_at: p.expires_at,
        }
    }
}

#[derive(Serialize)]
pub(crate) struct ShareSettingsDto {
    editors_can_share: bool,
    viewers_can_download: bool,
}

impl From<ShareSettings> for ShareSettingsDto {
    fn from(s: ShareSettings) -> Self {
        Self {
            editors_can_share: s.editors_can_share,
            viewers_can_download: s.viewers_can_download,
        }
    }
}

#[derive(Serialize)]
struct LinkStateDto<'a> {
    enabled: bool,
    role: Option<&'static str>,
    #[serde(serialize_with = "time::serde::rfc3339::option::serialize")]
    expires_at: Option<OffsetDateTime>,
    /// Absent, not null, for everyone the service gives no token to.
    #[serde(skip_serializing_if = "Option::is_none")]
    token: Option<&'a str>,
}

#[derive(Serialize)]
pub(crate) struct PermissionsDto<'a> {
    role: &'static str,
    owner: PersonDto<'a>,
    permissions: Vec<PermissionDto<'a>>,
    total: i64,
    settings: ShareSettingsDto,
    link: LinkStateDto<'a>,
    can_share: bool,
}

impl<'a> From<&'a Permissions> for PermissionsDto<'a> {
    fn from(p: &'a Permissions) -> Self {
        Self {
            role: p.role.as_str(),
            owner: PersonDto::from(&p.owner),
            permissions: p.permissions.iter().map(PermissionDto::from).collect(),
            total: p.total,
            settings: ShareSettingsDto::from(p.settings),
            link: LinkStateDto {
                enabled: p.link.enabled,
                role: p.link.enabled.then_some("viewer"),
                expires_at: p.link.expires_at.filter(|_| p.link.enabled),
                token: p.link.token.as_deref().filter(|_| p.link.enabled),
            },
            can_share: p.can_share,
        }
    }
}

/// The dialog after sharing by email, and how many invite mails the daily cap held back.
#[derive(Serialize)]
pub(crate) struct AddedPermissionsDto<'a> {
    #[serde(flatten)]
    permissions: PermissionsDto<'a>,
    mail_skipped: u32,
}

impl<'a> From<&'a AddedPermissions> for AddedPermissionsDto<'a> {
    fn from(a: &'a AddedPermissions) -> Self {
        Self {
            permissions: PermissionsDto::from(&a.permissions),
            mail_skipped: a.mail_skipped,
        }
    }
}

/// Only sharers receive the token: here and in their `LinkState`.
#[derive(Serialize)]
pub(crate) struct LinkCreatedDto<'a> {
    token: &'a str,
    role: &'static str,
    #[serde(serialize_with = "time::serde::rfc3339::option::serialize")]
    expires_at: Option<OffsetDateTime>,
}

impl<'a> From<&'a LinkCreated> for LinkCreatedDto<'a> {
    fn from(l: &'a LinkCreated) -> Self {
        Self {
            token: &l.token,
            role: "viewer",
            expires_at: l.expires_at,
        }
    }
}

#[derive(Serialize)]
struct SharedDocumentDto<'a> {
    #[serde(flatten)]
    document: DocumentDto<'a>,
    #[serde(serialize_with = "time::serde::rfc3339::serialize")]
    shared_at: OffsetDateTime,
    new: bool,
}

impl<'a> From<&'a SharedDocument> for SharedDocumentDto<'a> {
    fn from(s: &'a SharedDocument) -> Self {
        Self {
            document: DocumentDto::from(&s.document),
            shared_at: s.shared_at,
            new: s.new,
        }
    }
}

/// `listShared`.
#[derive(Serialize)]
pub(crate) struct SharedDto<'a> {
    documents: Vec<SharedDocumentDto<'a>>,
    unseen: u64,
    next_cursor: Option<&'a str>,
}

impl<'a> From<&'a Page<SharedList>> for SharedDto<'a> {
    fn from(p: &'a Page<SharedList>) -> Self {
        Self {
            documents: p
                .items
                .documents
                .iter()
                .map(SharedDocumentDto::from)
                .collect(),
            unseen: p.items.unseen,
            next_cursor: p.next_cursor.as_deref(),
        }
    }
}
