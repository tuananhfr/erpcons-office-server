use std::sync::Arc;

use uuid::Uuid;

use crate::{ByeReason, Hub, MemberId, Mode, PresenceState};

/// A session's place in a room; dropping it leaves the room, however the session ended.
pub struct Membership {
    hub: Arc<Hub>,
    document: Uuid,
    id: MemberId,
}

impl Membership {
    pub(crate) fn new(hub: Arc<Hub>, document: Uuid, id: MemberId) -> Self {
        Self { hub, document, id }
    }

    pub fn id(&self) -> MemberId {
        self.id
    }

    pub fn document(&self) -> Uuid {
        self.document
    }

    /// The member's role now allows `allowed` (after a re-check).
    pub fn set_allowed(&self, allowed: Mode) {
        self.hub.set_allowed(self.document, self.id, allowed);
    }

    /// Call on taking up an `Event::Recheck`, before reading the role: a change committed while it
    /// is read queues the next one.
    pub fn recheck_started(&self) {
        self.hub.recheck_started(self.document, self.id);
    }

    pub fn set_state(&self, state: PresenceState) {
        self.hub.set_state(self.document, self.id, state);
    }

    /// The member may no longer open the document: its session gets `bye` and closes.
    pub fn revoke(&self) {
        self.hub.close(self.document, self.id, ByeReason::Revoked);
    }
}

impl Drop for Membership {
    fn drop(&mut self) {
        self.hub.leave(self.document, self.id);
    }
}
