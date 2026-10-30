//! Document service: folders, documents and their immutable versions, trash, quota, upload checks,
//! sharing, and the purge and orphan jobs. Every access decision goes through [`authorize`].
mod authorize;
mod cursor;
mod email;
mod error;
mod fail_points;
mod format;
pub mod intake;
pub mod jobs;
mod link_cipher;
mod link_token;
mod model;
mod names;
mod quota;
mod service;
mod sharing_model;
mod templates;

pub use authorize::{Action, Denied, Resource, Role, authorize};
pub use email::{Email, InvalidEmail};
pub use error::{BodyTooLarge, DocError};
pub use fail_points::{FailPoint, FailPoints, Held};
pub use format::{Format, Kind};
pub use link_cipher::{InvalidLinkKey, LinkKey};
pub use model::{Document, Entries, Folder, FolderPath, Page, PageRequest, Version};
pub use names::{InvalidName, Name};
pub use quota::{QuotaConfig, QuotaError};
pub use service::{
    AddPermissions, Caller, Content, CreateDocument, DocumentPatch, Documents, FolderPatch,
    PermissionPatch, Staged,
};
pub use sharing_model::{
    AddedPermissions, LinkCreated, LinkState, Permission, Permissions, Person, ShareSettings,
    SharedDocument, SharedList,
};
pub use templates::{blank as blank_template, default_title};
