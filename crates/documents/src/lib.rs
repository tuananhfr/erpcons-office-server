//! Document service: folders, documents and their immutable versions, trash, quota, upload checks,
//! and the purge and orphan jobs. Every access decision goes through [`authorize`].
mod authorize;
mod cursor;
mod error;
mod fail_points;
mod format;
pub mod intake;
pub mod jobs;
mod model;
mod names;
mod quota;
mod service;
mod templates;

pub use authorize::{Action, NotFound, Resource, authorize};
pub use error::{BodyTooLarge, DocError};
pub use fail_points::{FailPoint, FailPoints, Held};
pub use format::{Format, Kind};
pub use model::{Document, Entries, Folder, Page, PageRequest, Version};
pub use names::{InvalidName, Name};
pub use quota::{QuotaConfig, QuotaError};
pub use service::{Caller, Content, CreateDocument, DocumentPatch, Documents, FolderPatch, Staged};
pub use templates::{blank as blank_template, default_title};
