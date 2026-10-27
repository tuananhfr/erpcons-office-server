use sqlx::PgPool;
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActorKind {
    User,
    System,
}

impl ActorKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::System => "system",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Success,
    Denied,
    Error,
}

impl Outcome {
    fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Denied => "denied",
            Self::Error => "error",
        }
    }
}

/// One audit row. Holds identifiers only: never titles, names, content or tokens (spec §12).
#[derive(Clone, Copy, Debug)]
pub struct AuditEvent<'a> {
    pub actor_kind: ActorKind,
    pub actor_id: Option<&'a str>,
    pub tenant_id: Option<&'a str>,
    pub document_id: Option<Uuid>,
    pub action: &'static str,
    pub outcome: Outcome,
    pub trace_id: &'a str,
}

pub async fn insert(pool: &PgPool, event: &AuditEvent<'_>) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO audit_events
             (id, actor_kind, actor_id, tenant_id, document_id, action, result, trace_id)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(Uuid::now_v7())
    .bind(event.actor_kind.as_str())
    .bind(event.actor_id)
    .bind(event.tenant_id)
    .bind(event.document_id)
    .bind(event.action)
    .bind(event.outcome.as_str())
    .bind(event.trace_id)
    .execute(pool)
    .await
    .map(drop)
}
