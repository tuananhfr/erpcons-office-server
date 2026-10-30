use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// `mail_outbox.kind` of [`SharedNotice`].
pub const KIND_SHARED: &str = "shared";

/// "Someone shared a document with you". Stored as the outbox payload. No `Debug`: names and the
/// message are personal data.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SharedNotice {
    pub document_id: Uuid,
    /// `word`, `sheet`, `slide` or `pdf`: picks the editor the link opens.
    pub kind: String,
    /// `editor`, `commenter` or `viewer`.
    pub role: String,
    pub sharer_name: String,
    /// The sharer's address as PI verified it, so the recipient can tell who really writes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sharer_email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// Only when the server is configured to include titles.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

impl SharedNotice {
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    pub fn from_json(json: &str) -> Option<Self> {
        serde_json::from_str(json).ok()
    }
}
