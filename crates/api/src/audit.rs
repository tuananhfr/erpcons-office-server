use office_db::audit::{self, AuditEvent};
use tracing::error;

use crate::AppState;

/// Writes an audit row for an action that changes no data (sign-in, `/me`). A failed write is logged,
/// not returned: refusing would only hide that the database is down. Mutations audit in their own
/// transaction (`office-documents`).
pub(crate) async fn record(state: &AppState, event: &AuditEvent<'_>) {
    if let Err(e) = audit::insert(&state.db, event).await {
        error!(action = event.action, error = %e, "audit write failed");
    }
}
