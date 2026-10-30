//! Writing one permission or invitation, with the rules on expiries: only the owner sets or lifts
//! them, and whatever a time-limited sharer grants ends no later than its own access.
use office_db::{PgConnection, acl};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{DocError, Email, Role};

/// Who grants on which document, and in what role.
#[derive(Clone, Copy)]
pub(super) struct Grantor<'a> {
    pub actor_id: &'a str,
    pub document_id: Uuid,
    pub owner: bool,
    /// When the grantor's own permission ends (never for the owner).
    pub own_expiry: Option<OffsetDateTime>,
    pub role: Role,
}

/// What a grant did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Granted {
    Unchanged,
    Changed,
    /// A new row: a recipient the grantor had not given access to here.
    New,
}

/// Why one sharing attempt stopped: a race worth one retry, or a real failure.
pub(super) enum Attempt {
    Collision,
    Failed(DocError),
}

impl Attempt {
    fn from_insert(err: office_db::Error) -> Self {
        if acl::is_collision(&err) {
            Self::Collision
        } else {
            Self::Failed(DocError::db(err))
        }
    }
}

impl From<DocError> for Attempt {
    fn from(err: DocError) -> Self {
        Self::Failed(err)
    }
}

impl From<Attempt> for DocError {
    fn from(attempt: Attempt) -> Self {
        match attempt {
            Attempt::Collision => DocError::Contended,
            Attempt::Failed(err) => err,
        }
    }
}

/// The earlier of two ends, `None` being never.
pub(super) fn earliest(
    a: Option<OffsetDateTime>,
    b: Option<OffsetDateTime>,
) -> Option<OffsetDateTime> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

/// Editor above commenter above viewer.
pub(super) fn rank(role: &str) -> u8 {
    match role {
        "editor" => 3,
        "commenter" => 2,
        "viewer" => 1,
        _ => 0,
    }
}

/// The end of a permission whose role a sharer changes: a raise ends no later than the sharer's
/// own access, so a time-limited sharer cannot hand out more than it holds.
pub(super) fn after_role_change(
    current: Option<OffsetDateTime>,
    from: &str,
    to: &str,
    own_expiry: Option<OffsetDateTime>,
) -> Option<OffsetDateTime> {
    if rank(to) > rank(from) {
        earliest(current, own_expiry)
    } else {
        current
    }
}

/// Grants the role to an account, recording the address it was invited by.
pub(super) async fn grant_actor(
    conn: &mut PgConnection,
    g: Grantor<'_>,
    actor_id: &str,
    email: &Email,
) -> Result<Granted, Attempt> {
    let existing = acl::existing_for_actor(&mut *conn, g.document_id, actor_id)
        .await
        .map_err(DocError::db)?;
    grant(conn, g, existing, Some(actor_id), email).await
}

/// Invites an address no account has (recently) verified.
pub(super) async fn grant_pending(
    conn: &mut PgConnection,
    g: Grantor<'_>,
    email: &Email,
) -> Result<Granted, Attempt> {
    let existing = acl::existing_pending(&mut *conn, g.document_id, email.as_str())
        .await
        .map_err(DocError::db)?;
    grant(conn, g, existing, None, email).await
}

async fn grant(
    conn: &mut PgConnection,
    g: Grantor<'_>,
    existing: Option<acl::Existing>,
    actor_id: Option<&str>,
    email: &Email,
) -> Result<Granted, Attempt> {
    let role = g.role.as_str();
    match existing {
        Some(e) if e.role == role && !e.expired => Ok(Granted::Unchanged),
        // Granting again would undo the expiry the owner set.
        Some(e) if e.expired && !g.owner => Err(DocError::Forbidden.into()),
        Some(e) => {
            // The owner's repeated invitation is a fresh one, without expiry.
            let expires_at = if g.owner {
                None
            } else {
                after_role_change(e.expires_at, &e.role, role, g.own_expiry)
            };
            acl::renew(conn, e.id, role, expires_at)
                .await
                .map_err(DocError::db)?;
            Ok(Granted::Changed)
        }
        None => {
            let new = acl::NewPermission {
                id: Uuid::now_v7(),
                document_id: g.document_id,
                actor_id,
                email_norm: Some(email.as_str()),
                role,
                granted_by: g.actor_id,
                expires_at: if g.owner { None } else { g.own_expiry },
            };
            acl::insert(conn, &new)
                .await
                .map_err(Attempt::from_insert)?;
            Ok(Granted::New)
        }
    }
}

#[cfg(test)]
mod tests {
    use time::Duration;

    use super::*;

    #[test]
    fn a_raise_never_outlives_the_sharer() {
        let now = OffsetDateTime::now_utc();
        let soon = Some(now + Duration::hours(1));
        let later = Some(now + Duration::days(9));
        assert_eq!(earliest(None, soon), soon);
        assert_eq!(earliest(later, soon), soon);
        assert_eq!(earliest(None, None), None);
        assert_eq!(after_role_change(None, "viewer", "editor", soon), soon);
        assert_eq!(after_role_change(later, "viewer", "commenter", soon), soon);
        // Lowering a role or a sharer without expiry changes no end.
        assert_eq!(after_role_change(None, "editor", "viewer", soon), None);
        assert_eq!(after_role_change(later, "viewer", "editor", None), later);
    }
}
