use std::sync::{Arc, atomic::AtomicUsize};

use uuid::Uuid;

use crate::{ByeReason, Frame, Hub, MemberId, Mode, PresenceState, member::unsent, room::Replay};

/// A session's place in a room; dropping it leaves the room, however the session ended.
pub struct Membership {
    hub: Arc<Hub>,
    document: Uuid,
    id: MemberId,
    /// The bytes of messages queued for this member and not yet written.
    backlog: Arc<AtomicUsize>,
}

impl Membership {
    pub(crate) fn new(
        hub: Arc<Hub>,
        document: Uuid,
        id: MemberId,
        backlog: Arc<AtomicUsize>,
    ) -> Self {
        Self {
            hub,
            document,
            id,
            backlog,
        }
    }

    /// The session wrote `frame`, taken off this member's queue: it no longer counts against the
    /// queue's bytes.
    pub fn written(&self, frame: &Frame) {
        unsent(&self.backlog, frame.weight());
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

    /// `lock.acquire`: answered `lock.granted`, `lock.denied` or `error` through the queue.
    pub fn acquire_lock(&self, region: &str, cid: &str) {
        self.hub.acquire_lock(self.document, self.id, region, cid);
    }

    pub fn renew_lock(&self, region: &str) {
        self.hub.renew_lock(self.document, self.id, region);
    }

    pub fn release_lock(&self, region: &str) {
        self.hub.release_lock(self.document, self.id, region);
    }

    /// Drops the room's lapsed locks; sessions call it often, as nothing else keeps time.
    pub fn expire_locks(&self) {
        self.hub.expire_locks(self.document);
    }

    /// `region.update`: answered `ack` (or `error`) through the queue, relayed to the others.
    pub fn update(&self, region: &str, cid: &str, payload: &str) {
        self.hub
            .update(self.document, self.id, region, cid, payload);
    }

    pub fn cursor(&self, region: &str, range: &str) {
        self.hub.cursor(self.document, self.id, region, range);
    }

    /// `region.replay`: the messages to send straight to the socket, in order.
    pub fn replay(&self, since: u64) -> Vec<Frame> {
        match self.hub.replay(self.document, self.id, since) {
            Some(Replay::Updates { mut updates, done }) => {
                updates.push(done);
                updates
            }
            Some(Replay::Gone(error)) => vec![error],
            None => Vec::new(),
        }
    }
}

impl Drop for Membership {
    fn drop(&mut self) {
        self.hub.leave(self.document, self.id);
    }
}
