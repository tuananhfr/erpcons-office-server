//! All rooms of this node, and the connection counts per client network, per account and in all.
//! One lock guards them: every operation is a few map updates and non-blocking sends.
use std::{
    collections::HashMap,
    net::IpAddr,
    sync::{Arc, Mutex, MutexGuard, PoisonError},
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{
    ByeReason, Event, LiveConfig, MemberId, Membership, Mode, Participant, PresenceState,
    quota_network,
    room::{Replay, Room},
};

/// Why a connection could not join.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JoinRefused {
    /// The document's room, or its share for link holders, is full.
    RoomFull,
    AddressFull,
    ActorFull,
    ServerFull,
    ShuttingDown,
}

#[derive(Default)]
struct State {
    rooms: HashMap<Uuid, Room>,
    per_address: HashMap<Option<IpAddr>, usize>,
    per_actor: HashMap<String, usize>,
    total: usize,
    /// Member ids grow over the whole node, so join order within a room follows them too.
    next_member: u64,
    /// The highest seq of a room that is gone; a new room numbers above it.
    retired_seq: u64,
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
        // Members keep only the network, which is all the count needs.
        let address = quota_network(address);
        let mut state = self.lock();
        if state.total >= self.config.max_connections {
            return Err(JoinRefused::ServerFull);
        }
        if state.per_address.get(&address).copied().unwrap_or(0) >= self.config.max_per_address {
            return Err(JoinRefused::AddressFull);
        }
        let actor = participant.actor_id().map(str::to_owned);
        if let Some(actor) = &actor
            && state.per_actor.get(actor).copied().unwrap_or(0) >= self.config.max_per_actor
        {
            return Err(JoinRefused::ActorFull);
        }
        state.next_member += 1;
        let id = MemberId::new(state.next_member);
        let base = state.retired_seq.max(clock_micros());
        let room = state
            .rooms
            .entry(document)
            .or_insert_with(|| Room::new(base));
        let links_full = matches!(participant, Participant::Link)
            && room.links() >= self.config.max_links_per_document;
        if room.len() >= self.config.max_per_document || links_full {
            if room.is_empty() {
                state.rooms.remove(&document);
            }
            return Err(JoinRefused::RoomFull);
        }
        let (sender, receiver) = mpsc::channel(self.config.outbox.max(1));
        room.add(&self.config, id, participant, allowed, address, sender);
        let removed = room.take_removed();
        state.total += 1;
        *state.per_address.entry(address).or_insert(0) += 1;
        if let Some(actor) = actor {
            *state.per_actor.entry(actor).or_insert(0) += 1;
        }
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

    pub(crate) fn acquire_lock(&self, document: Uuid, member: MemberId, region: &str, cid: &str) {
        self.with_room(document, |room, config| {
            room.acquire_lock(config, member, region, cid, Instant::now());
        });
    }

    pub(crate) fn renew_lock(&self, document: Uuid, member: MemberId, region: &str) {
        self.with_room(document, |room, config| {
            room.renew_lock(config, member, region, Instant::now());
        });
    }

    pub(crate) fn release_lock(&self, document: Uuid, member: MemberId, region: &str) {
        self.with_room(document, |room, config| {
            room.release_lock(config, member, region);
        });
    }

    pub(crate) fn expire_locks(&self, document: Uuid) {
        self.with_room(document, |room, config| {
            room.expire_locks(config, Instant::now());
        });
    }

    pub(crate) fn update(
        &self,
        document: Uuid,
        member: MemberId,
        region: &str,
        cid: &str,
        payload: &str,
    ) {
        self.with_room(document, |room, config| {
            room.update(config, member, region, cid, payload, Instant::now());
        });
    }

    pub(crate) fn cursor(&self, document: Uuid, member: MemberId, region: &str, range: &str) {
        self.with_room(document, |room, config| {
            room.cursor(config, member, region, range);
        });
    }

    pub(crate) fn replay(&self, document: Uuid, member: MemberId, since: u64) -> Option<Replay> {
        self.lock()
            .rooms
            .get(&document)
            .and_then(|room| room.replay(member, since))
    }

    /// `actor_id` saved `document` as `version`, holding every room update up to `live_seq`
    /// (`X-Live-Seq`); counts only from the room's leader (see `Room::saved`).
    pub fn saved(&self, document: Uuid, actor_id: &str, live_seq: u64, version: i32) {
        self.with_room(document, |room, config| {
            room.saved(config, actor_id, live_seq, version);
        });
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

    /// Region locks in the document's room, lapsed ones not yet swept included.
    pub fn locks_held(&self, document: Uuid) -> usize {
        self.lock()
            .rooms
            .get(&document)
            .map_or(0, |room| room.locks().len())
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
            if let Some(actor) = member.participant.actor_id()
                && let Some(count) = state.per_actor.get_mut(actor)
            {
                *count = count.saturating_sub(1);
                if *count == 0 {
                    state.per_actor.remove(actor);
                }
            }
        }
        if state.rooms.get(&document).is_some_and(Room::is_empty)
            && let Some(room) = state.rooms.remove(&document)
        {
            state.retired_seq = state.retired_seq.max(room.seq());
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Microseconds since the Unix epoch: a room opened after a restart numbers above anything the
/// previous process could have issued, so a client's old `since` never matches a new update.
/// Below 2^53 (exact in JavaScript) until the year 2255.
fn clock_micros() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_micros()).unwrap_or(u64::MAX))
}
