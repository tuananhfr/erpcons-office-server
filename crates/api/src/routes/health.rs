use std::time::Duration;

use axum::{Json, extract::State};
use serde::Serialize;
use tokio::time::timeout;
use tracing::warn;

use crate::{ApiError, AppState, ErrorCode};

/// A probe slower than this counts as down, so the deploy check gets an answer quickly.
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Serialize)]
pub(crate) struct Health {
    status: &'static str,
}

/// Ready when the database and storage both answer. Says nothing else, since it is public.
pub(crate) async fn get(State(state): State<AppState>) -> Result<Json<Health>, ApiError> {
    let (db, storage) = tokio::join!(
        timeout(PROBE_TIMEOUT, office_db::ping(&state.db)),
        timeout(PROBE_TIMEOUT, state.storage.health()),
    );
    let db_ok = matches!(db, Ok(Ok(())));
    let storage_ok = matches!(storage, Ok(Ok(())));
    if !db_ok {
        warn!("health: database unreachable");
    }
    if !storage_ok {
        warn!("health: storage unavailable");
    }
    if db_ok && storage_ok {
        Ok(Json(Health { status: "ok" }))
    } else {
        Err(ErrorCode::Unavailable.into())
    }
}
