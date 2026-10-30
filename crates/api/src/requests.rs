//! Request bodies and query strings of the contract.
use office_documents::{PageRequest, Role};
use serde::{Deserialize, Deserializer, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{ApiError, ErrorCode};

const DEFAULT_LIMIT: u32 = 50;
const MAX_LIMIT: u32 = 200;
const MAX_CURSOR_LEN: usize = 200;

/// `cursor` and `limit` (1–200, default 50).
#[derive(Deserialize)]
pub(crate) struct PageQuery {
    cursor: Option<String>,
    limit: Option<u32>,
}

impl PageQuery {
    pub(crate) fn page(self) -> Result<PageRequest, ApiError> {
        let limit = self.limit.unwrap_or(DEFAULT_LIMIT);
        let cursor_ok = self
            .cursor
            .as_ref()
            .is_none_or(|c| !c.is_empty() && c.len() <= MAX_CURSOR_LEN);
        if !(1..=MAX_LIMIT).contains(&limit) || !cursor_ok {
            return Err(ErrorCode::InvalidRequest.into());
        }
        Ok(PageRequest {
            cursor: self.cursor,
            limit,
        })
    }
}

/// Tells an absent field (`None`) from an explicit `null` (`Some(None)`, "move to the root").
fn present<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
    d: D,
) -> Result<Option<Option<T>>, D::Error> {
    Option::<T>::deserialize(d).map(Some)
}

#[derive(Deserialize, Serialize)]
pub(crate) struct CreateFolder {
    #[serde(default)]
    pub parent_id: Option<Uuid>,
    pub name: String,
}

#[derive(Deserialize)]
pub(crate) struct UpdateFolder {
    pub name: Option<String>,
    #[serde(default, deserialize_with = "present")]
    pub parent_id: Option<Option<Uuid>>,
}

#[derive(Deserialize, Serialize)]
pub(crate) struct CreateBlank {
    pub kind: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub folder_id: Option<Uuid>,
}

#[derive(Deserialize)]
pub(crate) struct UpdateDocument {
    pub title: Option<String>,
    #[serde(default, deserialize_with = "present")]
    pub folder_id: Option<Option<Uuid>>,
}

/// As [`present`], for an RFC 3339 time.
fn present_time<'de, D: Deserializer<'de>>(
    d: D,
) -> Result<Option<Option<OffsetDateTime>>, D::Error> {
    time::serde::rfc3339::option::deserialize(d).map(Some)
}

/// `editor`, `commenter` or `viewer`; anything else is 422.
pub(crate) fn grant_role(role: &str) -> Result<Role, ApiError> {
    Role::parse_grant(role).ok_or_else(|| ErrorCode::InvalidRequest.into())
}

#[derive(Deserialize)]
pub(crate) struct AddPermissions {
    pub emails: Vec<String>,
    pub role: String,
    pub notify: bool,
    #[serde(default)]
    pub message: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct UpdatePermission {
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default, deserialize_with = "present_time")]
    pub expires_at: Option<Option<OffsetDateTime>>,
}

#[derive(Deserialize)]
pub(crate) struct PutShareSettings {
    pub editors_can_share: bool,
    pub viewers_can_download: bool,
}

/// `expires_at` absent is `None`, `null` is `Some(None)`: a sharer other than the owner may send
/// the link's current end, or none, but not another.
#[derive(Default, Deserialize)]
pub(crate) struct EnableLink {
    #[serde(default, deserialize_with = "present_time")]
    pub expires_at: Option<Option<OffsetDateTime>>,
}
