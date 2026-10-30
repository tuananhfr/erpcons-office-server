//! What the sharing operations return. No `Debug` anywhere: names and emails are personal data.
use office_db::acl::PermissionRow;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{DocError, Document, Role};

#[derive(Clone)]
pub struct Person {
    pub actor_id: String,
    pub display_name: String,
    pub email: Option<String>,
}

#[derive(Clone)]
pub struct Permission {
    pub id: Uuid,
    pub actor_id: Option<String>,
    pub display_name: Option<String>,
    pub email: Option<String>,
    pub role: Role,
    pub created_at: OffsetDateTime,
    pub expires_at: Option<OffsetDateTime>,
}

impl Permission {
    pub fn pending(&self) -> bool {
        self.actor_id.is_none()
    }
}

impl TryFrom<PermissionRow> for Permission {
    type Error = DocError;

    fn try_from(r: PermissionRow) -> Result<Self, DocError> {
        let role = Role::parse_grant(&r.role).ok_or_else(|| {
            tracing::error!(permission_id = %r.id, "permission has an unknown role");
            DocError::Unavailable
        })?;
        Ok(Self {
            id: r.id,
            actor_id: r.actor_id,
            display_name: r.display_name,
            email: r.email,
            role,
            created_at: r.created_at,
            expires_at: r.expires_at,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShareSettings {
    pub editors_can_share: bool,
    pub viewers_can_download: bool,
}

/// No `Debug`: may hold the token.
#[derive(Clone, PartialEq, Eq)]
pub struct LinkState {
    pub enabled: bool,
    pub expires_at: Option<OffsetDateTime>,
    /// Only for callers who may share, while the link is on.
    pub token: Option<String>,
}

/// The share dialog of one document, as the caller may see it.
#[derive(Clone)]
pub struct Permissions {
    pub role: Role,
    pub owner: Person,
    /// At most 500 (the cap per document), oldest first.
    pub permissions: Vec<Permission>,
    /// How many there are in all.
    pub total: i64,
    pub settings: ShareSettings,
    pub link: LinkState,
    pub can_share: bool,
}

/// The link's token, new or the one already on. No `Debug`: it opens the document.
#[derive(Clone)]
pub struct LinkCreated {
    pub token: String,
    pub expires_at: Option<OffsetDateTime>,
}

/// What sharing by email did, besides the dialog's new state.
#[derive(Clone)]
pub struct AddedPermissions {
    pub permissions: Permissions,
    /// Mails not queued because the caller reached its daily cap.
    pub mail_skipped: u32,
}

/// A document in "Shared with me".
#[derive(Clone)]
pub struct SharedDocument {
    pub document: Document,
    pub shared_at: OffsetDateTime,
    pub new: bool,
}

/// One page of "Shared with me" and how many of all are still new.
#[derive(Clone)]
pub struct SharedList {
    pub documents: Vec<SharedDocument>,
    pub unseen: u64,
}
