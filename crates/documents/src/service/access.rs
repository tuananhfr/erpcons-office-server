//! Loads a document with the caller's role and decides, through [`authorize`], whether the caller
//! may do `action`. Read from the database on every request: no cache, so a removed permission is
//! refused at the very next request.
use office_db::{ItemState, PgConnection, PgExecutor, access::AccessRow, documents::DocumentRow};
use office_identity::Actor;
use time::OffsetDateTime;
use uuid::Uuid;

use super::Caller;
use crate::{Action, DocError, Document, Resource, Role, authorize};

/// A document the caller may act on, with how it may. No `Debug`: holds the title.
pub(super) struct Accessed {
    pub row: DocumentRow,
    pub role: Role,
    /// When the caller's own permission ends; `None` for the owner, a link, or no expiry.
    pub grant_expires_at: Option<OffsetDateTime>,
    pub editors_can_share: bool,
    pub viewers_can_download: bool,
}

impl Accessed {
    pub fn owner_id(&self) -> &str {
        &self.row.owner_id
    }

    pub fn document(self) -> Result<Document, DocError> {
        Document::from_row(
            self.row,
            self.role,
            self.viewers_can_download,
            self.editors_can_share,
        )
    }

    /// Whether the caller, already let in, may also do `action`.
    pub fn allows(&self, actor: &Actor, action: Action) -> Result<(), DocError> {
        authorize(Some(actor), self.resource(), action).map_err(DocError::from)
    }

    fn resource(&self) -> Resource<'static> {
        Resource::Document {
            role: Some(self.role),
            trashed: self.row.state == ItemState::Trashed,
            editors_can_share: self.editors_can_share,
        }
    }
}

/// Decides on a loaded row. Drafts and purged documents are as missing as unknown ids; a trashed
/// one is the owner's alone.
pub(super) fn decide(
    row: Option<AccessRow>,
    actor: Option<&Actor>,
    link: bool,
    action: Action,
) -> Result<Accessed, DocError> {
    let row = row.ok_or(DocError::NotFound)?;
    let state = row.document.state;
    if !matches!(state, ItemState::Active | ItemState::Trashed) {
        return Err(DocError::NotFound);
    }
    let grant = if link {
        Some(Role::Link)
    } else {
        row.grant_role.as_deref().and_then(Role::parse_grant)
    };
    let role = Role::of(actor, &row.document.owner_id, grant);
    authorize(
        actor,
        Resource::Document {
            role,
            trashed: state == ItemState::Trashed,
            editors_can_share: row.editors_can_share,
        },
        action,
    )?;
    let grant_expires_at = if link { None } else { row.grant_expires_at };
    Ok(Accessed {
        role: role.ok_or(DocError::NotFound)?,
        grant_expires_at,
        row: row.document,
        editors_can_share: row.editors_can_share,
        viewers_can_download: row.viewers_can_download,
    })
}

/// The document `id` if the caller may do `action` on it (active or, for its owner, trashed).
pub(super) async fn load(
    db: impl PgExecutor<'_>,
    c: &Caller<'_>,
    id: Uuid,
    action: Action,
) -> Result<Accessed, DocError> {
    let row = office_db::access::of_actor(db, id, c.actor_id())
        .await
        .map_err(DocError::db)?;
    decide(row, Some(c.actor), false, action)
}

/// As [`load`], and not in the trash: a trashed document can be read, not changed.
pub(super) async fn load_active(
    db: impl PgExecutor<'_>,
    c: &Caller<'_>,
    id: Uuid,
    action: Action,
) -> Result<Accessed, DocError> {
    let accessed = load(db, c, id, action).await?;
    active(accessed)
}

/// As [`load_active`], with the document row locked until the transaction ends: the decision holds
/// until the commit, and concurrent changes of this document wait for it.
pub(super) async fn load_for_update(
    conn: &mut PgConnection,
    c: &Caller<'_>,
    id: Uuid,
    action: Action,
) -> Result<Accessed, DocError> {
    let row = office_db::access::of_actor_for_update(conn, id, c.actor_id())
        .await
        .map_err(DocError::db)?;
    active(decide(row, Some(c.actor), false, action)?)
}

fn active(accessed: Accessed) -> Result<Accessed, DocError> {
    if accessed.row.state != ItemState::Active {
        return Err(DocError::NotFound);
    }
    Ok(accessed)
}
