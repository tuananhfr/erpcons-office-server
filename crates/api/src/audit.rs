use office_db::audit::{self, AuditEvent};
use tracing::error;

use crate::AppState;

/// Writes an audit row. A failed write is logged, not returned: in milestone 1 no audited action
/// changes data, so refusing the request would only hide that the database is down.
pub(crate) async fn record(state: &AppState, event: &AuditEvent<'_>) {
    if let Err(e) = audit::insert(&state.db, event).await {
        error!(action = event.action, error = %e, "audit write failed");
    }
}
