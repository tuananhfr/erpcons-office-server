//! Document service. Phase 1, milestone 1: the authorization seam and the quota rules; the
//! document and folder operations come in milestone 2.
mod authorize;
mod quota;

pub use authorize::{Action, NotFound, Resource, authorize};
pub use quota::{QuotaConfig, QuotaError};
