use office_db::{
    PgConnection,
    audit::{self, ActorKind, AuditEvent, Outcome},
};
use tracing::error;
use uuid::Uuid;

use super::{Caller, Documents};
use crate::DocError;

/// What an audit row is about; ids only.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Subject {
    /// Nothing exists yet (a create that failed).
    None,
    Document(Uuid),
    Folder(Uuid),
}

impl Subject {
    fn ids(self) -> (Option<Uuid>, Option<Uuid>) {
        match self {
            Self::None => (None, None),
            Self::Document(id) => (Some(id), None),
            Self::Folder(id) => (None, Some(id)),
        }
    }
}

/// Writes the success row inside the mutation's transaction: both commit, or neither.
pub(super) async fn success(
    conn: &mut PgConnection,
    c: &Caller<'_>,
    action: &'static str,
    subject: Subject,
) -> Result<(), DocError> {
    let (document_id, folder_id) = subject.ids();
    let event = AuditEvent {
        actor_kind: ActorKind::User,
        actor_id: Some(c.actor_id()),
        tenant_id: None,
        document_id,
        folder_id,
        action,
        outcome: Outcome::Success,
        trace_id: c.trace_id,
    };
    audit::insert(conn, &event).await.map_err(DocError::db)
}

/// The system's own row (the purge job), inside its transaction.
pub(crate) async fn system(
    conn: &mut PgConnection,
    action: &'static str,
    subject: Subject,
    trace_id: &str,
) -> Result<(), DocError> {
    let (document_id, folder_id) = subject.ids();
    let event = AuditEvent {
        actor_kind: ActorKind::System,
        actor_id: None,
        tenant_id: None,
        document_id,
        folder_id,
        action,
        outcome: Outcome::Success,
        trace_id,
    };
    audit::insert(conn, &event).await.map_err(DocError::db)
}

/// A view through the public link: no actor, and never the token.
pub(super) async fn link(
    db: impl office_db::PgExecutor<'_>,
    action: &'static str,
    document_id: Uuid,
    trace_id: &str,
) {
    let event = AuditEvent {
        actor_kind: ActorKind::Link,
        actor_id: None,
        tenant_id: None,
        document_id: Some(document_id),
        folder_id: None,
        action,
        outcome: Outcome::Success,
        trace_id,
    };
    // A view changes nothing; failing it because the audit write failed would only hide the cause.
    if let Err(e) = audit::insert(db, &event).await {
        error!(action, error = %e, "audit write failed");
    }
}

impl Documents {
    /// Records a refused or failed mutation. Best effort: the transaction is gone, and failing the
    /// request again because the audit write failed would hide the first error.
    pub(super) async fn audit_failure<T>(
        &self,
        c: &Caller<'_>,
        action: &'static str,
        subject: Subject,
        result: &Result<T, DocError>,
    ) {
        let Err(err) = result else {
            return;
        };
        let (document_id, folder_id) = subject.ids();
        let outcome = match err {
            DocError::NotFound | DocError::Forbidden => Outcome::Denied,
            _ => Outcome::Error,
        };
        let event = AuditEvent {
            actor_kind: ActorKind::User,
            actor_id: Some(c.actor_id()),
            tenant_id: None,
            document_id,
            folder_id,
            action,
            outcome,
            trace_id: c.trace_id,
        };
        if let Err(e) = audit::insert(&self.db, &event).await {
            error!(action, error = %e, "audit write failed");
        }
    }
}
