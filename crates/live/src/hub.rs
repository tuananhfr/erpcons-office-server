//! All rooms of this node, and the connection counts per address and in all. One lock guards
//! them: every operation is a few map updates and non-blocking sends.
use std::{
    collections::HashMap,
    net::IpAddr,
    sync::{Arc, Mutex, MutexGuard, PoisonError},
};

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{
    ByeReason, Event, LiveConfig, MemberId, Membership, Mode, Participant, PresenceState,
    room::Room,
};

/// Why a connection could not join.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JoinRefused {
    RoomFull,
    AddressFull,
    ServerFull,
    ShuttingDown,
}

#[derive(Default)]
struct State {
    rooms: HashMap<Uuid, Room>,
    per_address: HashMap<Option<IpAddr>, usize>,
    total: usize,
}

pub struct Hub {
    config: LiveConfig,
    state: Mutex<State>,
    shutdown: CancellationToken,
}

impl Hub {
    pub fn new(config: LiveConfig) -> Self {
        Self {
            config,
            state: Mutex::new(State::default()),
            shutdown: CancellationToken::new(),
        }
    }

    pub fn config(&self) -> &LiveConfig {
        &self.config
    }

    /// Adds a member that may use the document as `allowed` (the room may grant less). The member
    /// leaves when the returned [`Membership`] drops; its events arrive on the receiver.
    pub fn join(
        self: &Arc<Self>,
        document: Uuid,
        participant: Participant,
        allowed: Mode,
        address: Option<IpAddr>,
    ) -> Result<(Membership, mpsc::Receiver<Event>), JoinRefused> {
        if self.shutdown.is_cancelled() {
            return Err(JoinRefused::ShuttingDown);
        }
        let mut state = self.lock();
        if state.total >= self.config.max_connections {
            return Err(JoinRefused::ServerFull);
        }
        if state.per_address.get(&address).copied().unwrap_or(0) >= self.config.max_per_address {
            return Err(JoinRefused::AddressFull);
        }
        let room = state.rooms.entry(document).or_insert_with(Room::new);
        if room.len() >= self.config.max_per_document {
            if room.is_empty() {
                state.rooms.remove(&document);
            }
            return Err(JoinRefused::RoomFull);
        }
        let (sender, receiver) = mpsc::channel(self.config.outbox.max(1));
        let id = room.add(&self.config, participant, allowed, address, sender);
        let removed = room.take_removed();
        state.total += 1;
        *state.per_address.entry(address).or_insert(0) += 1;
        Self::release(&mut state, document, removed);
        Ok((Membership::new(self.clone(), document, id), receiver))
    }

    pub(crate) fn leave(&self, document: Uuid, member: MemberId) {
        self.with_room(document, |room, config| room.remove(config, member));
    }

    pub(crate) fn set_allowed(&self, document: Uuid, member: MemberId, allowed: Mode) {
        self.with_room(document, |room, config| {
            room.set_allowed(config, member, allowed);
        });
    }

    pub(crate) fn set_state(&self, document: Uuid, member: MemberId, state: PresenceState) {
        self.with_room(document, |room, config| {
            room.set_state(config, member, state);
        });
    }

    pub(crate) fn close(&self, document: Uuid, member: MemberId, reason: ByeReason) {
        self.with_room(document, |room, config| room.close(config, member, reason));
    }

    /// Access to `document` may have changed in this process: each member's session reads its
    /// role again at once.
    pub fn access_changed(&self, document: Uuid) {
        self.with_room(document, Room::recheck);
    }

    pub(crate) fn recheck_started(&self, document: Uuid, member: MemberId) {
        self.with_room(document, |room, _| room.recheck_started(member));
    }

    /// Whether a save of `document` by `actor_id` may go on (see `Room::may_save`).
    pub fn may_save(&self, document: Uuid, actor_id: &str) -> bool {
        self.lock()
            .rooms
            .get(&document)
            .is_none_or(|room| room.may_save(actor_id))
    }

    /// Ends every session (`bye` "shutdown") and refuses new ones.
    pub fn shutdown(&self) {
        self.shutdown.cancel();
    }

    /// Cancelled when the server shuts down.
    pub fn shutdown_token(&self) -> &CancellationToken {
        &self.shutdown
    }

    pub fn connections(&self) -> usize {
        self.lock().total
    }

    pub fn members_of(&self, document: Uuid) -> usize {
        self.lock().rooms.get(&document).map_or(0, Room::len)
    }

    /// Region locks held in the document's room (M4 fills them).
    pub fn locks_held(&self, document: Uuid) -> usize {
        self.lock()
            .rooms
            .get_mut(&document)
            .map_or(0, |room| room.locks_mut().len())
    }

    fn with_room(&self, document: Uuid, f: impl FnOnce(&mut Room, &LiveConfig)) {
        let mut state = self.lock();
        let Some(room) = state.rooms.get_mut(&document) else {
            return;
        };
        f(room, &self.config);
        let removed = room.take_removed();
        Self::release(&mut state, document, removed);
    }

    /// Releases the counts of removed members and forgets an empty room.
    fn release(state: &mut State, document: Uuid, removed: Vec<crate::member::Member>) {
        for member in removed {
            state.total = state.total.saturating_sub(1);
            if let Some(count) = state.per_address.get_mut(&member.address) {
                *count = count.saturating_sub(1);
                if *count == 0 {
                    state.per_address.remove(&member.address);
                }
            }
        }
        if state.rooms.get(&document).is_some_and(Room::is_empty) {
            state.rooms.remove(&document);
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}
