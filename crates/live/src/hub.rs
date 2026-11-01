//! All rooms of this node, and the connection counts per client network, per account and in all.
//! One lock guards them: every operation is a few map updates and non-blocking sends, and messages
//! are built so that serializing them happens outside it (see [`crate::Frame`]).
use std::{
    collections::HashMap,
    net::IpAddr,
    sync::{Arc, Mutex, MutexGuard, PoisonError, atomic::AtomicUsize},
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{
    ByeReason, Event, LiveConfig, MemberId, Membership, Mode, Participant, PresenceState,
    member::Outbox,
    protocol, quota_network,
    room::{Replay, Room, Update},
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
    /// Sum of the rooms' log memory (`Room::log_memory`).
    log_bytes: usize,
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
        let backlog = Arc::new(AtomicUsize::new(0));
        let outbox = Outbox {
            sender,
            backlog: backlog.clone(),
            cap: self.config.backlog_cap(),
        };
        room.add(&self.config, id, participant, allowed, address, outbox);
        let removed = room.take_removed();
        state.total += 1;
        *state.per_address.entry(address).or_insert(0) += 1;
        if let Some(actor) = actor {
            *state.per_actor.entry(actor).or_insert(0) += 1;
        }
        Self::release(&mut state, document, removed);
        Ok((
            Membership::new(self.clone(), document, id, backlog),
            receiver,
        ))
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

    /// A comment thread of `document` changed (`None`: several did).
    pub fn comments_changed(&self, document: Uuid, thread: Option<Uuid>) {
        self.with_room(document, |room, config| {
            room.comments_changed(config, thread);
        });
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
        // Escaping up to a MiB of payload is the costly part; the lock only adds the seq.
        let head: Arc<str> = protocol::region_update_head(region, member, payload).into();
        let now = Instant::now();
        let mut state = self.lock();
        let mut actor = None;
        self.in_room(&mut state, document, |room, config| {
            actor = room.check_update(config, member, region, cid, now);
        });
        let Some(actor) = actor else {
            return;
        };
        let Some(room) = state.rooms.get(&document) else {
            return;
        };
        let needed = room.log_cost(&self.config, head.len() + UPDATE_TAIL_BYTES);
        let room_left = self.make_log_room(&mut state, needed, now);
        self.in_room(&mut state, document, |room, config| {
            let update = Update {
                actor: &actor,
                cid,
                head,
            };
            room.apply_update(config, member, update, room_left, now);
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
            .get_mut(&document)
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

    /// The document room's leader, if any.
    pub fn leader(&self, document: Uuid) -> Option<MemberId> {
        self.lock().rooms.get(&document).and_then(Room::leader)
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
        self.in_room(&mut state, document, f);
    }

    fn in_room(&self, state: &mut State, document: Uuid, f: impl FnOnce(&mut Room, &LiveConfig)) {
        let Some(room) = state.rooms.get_mut(&document) else {
            return;
        };
        let before = room.log_memory();
        f(room, &self.config);
        let after = room.log_memory();
        let removed = room.take_removed();
        state.log_bytes = (state.log_bytes + after).saturating_sub(before);
        Self::release(state, document, removed);
    }

    /// Whether `needed` more bytes fit in `max_total_log_bytes`, after shedding the oldest updates
    /// of the rooms above their fair share (the budget split among open rooms, at least a sixteenth
    /// of a room's log), evened down from the largest. Rooms within their share keep their logs: a
    /// heavy room cannot make others lose replay, and when shedding is not enough the update waits.
    /// Rooms idle since their last save give back their remembered ids first, so a tenant whose
    /// rooms filled the node once cannot block everyone's edits for good.
    fn make_log_room(&self, state: &mut State, needed: usize, now: Instant) -> bool {
        let budget = self.config.max_total_log_bytes;
        if state.log_bytes + needed <= budget {
            return true;
        }
        let State {
            rooms, log_bytes, ..
        } = state;
        for room in rooms.values_mut() {
            let before = room.log_memory();
            if room.release_idle(now, self.config.idle_log_release) {
                *log_bytes = log_bytes.saturating_sub(before - room.log_memory());
            }
        }
        if state.log_bytes + needed <= budget {
            return true;
        }
        // A sixteenth below the budget, so a busy node does not shed on every update.
        let goal = budget.saturating_sub(needed).saturating_sub(budget / 16);
        let excess = state.log_bytes.saturating_sub(goal);
        let fair = (budget / state.rooms.len().max(1)).max(self.config.max_log_bytes / 16);
        let mut heavy: Vec<(usize, Uuid)> = state
            .rooms
            .iter()
            .map(|(document, room)| (room.log_memory(), *document))
            .filter(|(size, _)| *size > fair)
            .collect();
        heavy.sort_unstable_by(|a, b| b.cmp(a));
        let level = even_level(heavy.iter().map(|(size, _)| *size), excess, fair);
        for (size, document) in heavy {
            if size <= level {
                break;
            }
            let Some(room) = state.rooms.get_mut(&document) else {
                continue;
            };
            room.shed_log(&self.config, level);
            let after = room.log_memory();
            let removed = room.take_removed();
            state.log_bytes = (state.log_bytes + after).saturating_sub(size);
            Self::release(state, document, removed);
        }
        state.log_bytes + needed <= budget
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
            state.log_bytes = state.log_bytes.saturating_sub(room.log_memory());
            state.retired_seq = state.retired_seq.max(room.seq());
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Longest `region_update_tail`: `,"seq":` and a u64 and `}`.
const UPDATE_TAIL_BYTES: usize = 32;

/// The level to cut `sizes` (largest first) down to so that together they lose at least `excess`,
/// never below `floor`.
fn even_level(sizes: impl Iterator<Item = usize>, excess: usize, floor: usize) -> usize {
    let sizes: Vec<usize> = sizes.collect();
    let mut sum = 0;
    for (i, size) in sizes.iter().enumerate() {
        sum += size;
        let next = sizes.get(i + 1).copied().unwrap_or(floor).max(floor);
        let count = i + 1;
        if sum - next * count >= excess {
            return ((sum - excess) / count).max(floor);
        }
    }
    floor
}

/// Microseconds since the Unix epoch: a room opened after a restart numbers above anything the
/// previous process could have issued, so a client's old `since` never matches a new update.
/// Below 2^53 (exact in JavaScript) until the year 2255.
fn clock_micros() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_micros()).unwrap_or(u64::MAX))
}
