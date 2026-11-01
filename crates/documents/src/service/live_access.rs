//! Who may be in a document's live room, and how: the same decision as every request, through
//! [`crate::authorize`] with the role read from the database, never cached.
use office_identity::Actor;
use uuid::Uuid;

use super::{Caller, Documents, access, audit};
use crate::{Action, DocError, LinkHash, Role};

/// Someone asking to be, or staying, in a room.
#[derive(Clone, Copy)]
pub enum LiveViewer<'a> {
    Actor(&'a Actor),
    /// A holder of the public link, known only by the token's sha256.
    Link(&'a LinkHash),
}

/// The viewer's role, and whether the document is in its owner's trash (readable, not editable).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LiveAccess {
    pub role: Role,
    pub trashed: bool,
}

impl Documents {
    /// `NotFound` when the viewer may not open `id`, as for any read.
    pub async fn live_access(
        &self,
        id: Uuid,
        viewer: LiveViewer<'_>,
    ) -> Result<LiveAccess, DocError> {
        self.live_accessed(id, viewer)
            .await
            .map(|(access, _)| access)
    }

    /// The access and the document owner's id.
    async fn live_accessed(
        &self,
        id: Uuid,
        viewer: LiveViewer<'_>,
    ) -> Result<(LiveAccess, String), DocError> {
        let accessed = match viewer {
            LiveViewer::Actor(actor) => {
                let c = Caller {
                    actor,
                    trace_id: "",
                };
                access::load(&self.db, &c, id, Action::Read).await?
            }
            LiveViewer::Link(hash) => {
                let row = office_db::access::of_link(&self.db, hash.as_str())
                    .await
                    .map_err(DocError::db)?;
                let accessed = access::decide(row, None, true, Action::Read)?;
                // A link opens its own document only.
                if accessed.row.id != id {
                    return Err(DocError::NotFound);
                }
                accessed
            }
        };
        let access = LiveAccess {
            role: accessed.role,
            trashed: accessed.row.state == office_db::ItemState::Trashed,
        };
        Ok((access, accessed.row.owner_id))
    }

    /// As [`Self::live_access`] for a link holder about to join, audited like any link view. Also
    /// returns the document owner's id, which caps the link tickets of all its documents.
    pub async fn live_link_access(
        &self,
        id: Uuid,
        hash: &LinkHash,
        trace_id: &str,
    ) -> Result<(LiveAccess, String), DocError> {
        let accessed = self.live_accessed(id, LiveViewer::Link(hash)).await?;
        audit::link(&self.db, "link.live", id, trace_id).await;
        Ok(accessed)
    }

    /// Tells the live rooms that access to these documents changed. Called only after a commit
    /// that changed something: every call costs each member of the room a database read.
    pub(super) fn live_changed(&self, documents: impl IntoIterator<Item = Uuid>) {
        if let Some(rooms) = &self.live {
            documents
                .into_iter()
                .for_each(|id| rooms.access_changed(id));
        }
    }

    /// Tells the room of `id` that the caller's save of `version` holds its updates up to
    /// `live_seq`.
    pub(super) fn live_saved(&self, c: &Caller<'_>, id: Uuid, live_seq: u64, version: i32) {
        if let Some(rooms) = &self.live {
            rooms.saved(id, c.actor_id(), live_seq, version);
        }
    }

    /// Refuses a save while several members edit `id` and the caller is not their leader.
    pub(super) fn check_leader(&self, c: &Caller<'_>, id: Uuid) -> Result<(), DocError> {
        match &self.live {
            Some(rooms) if !rooms.may_save(id, c.actor_id()) => Err(DocError::NotLeader),
            _ => Ok(()),
        }
    }
}
