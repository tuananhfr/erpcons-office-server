//! One document's room: members in join order, the change sequence, editor slots, the leader,
//! the lock table and the log of updates since the last save. Every change sends what it changed
//! to the members it concerns; a member whose queue is full or whose session is gone is removed,
//! and [`Room::take_removed`] hands it back. Co-editing (locks, updates, cursors, replay, saves)
//! is in `editing`.
mod editing;

use std::{collections::BTreeMap, net::IpAddr, time::Instant};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use ring::hmac;
use tokio::sync::mpsc::{self, error::TrySendError};
use uuid::Uuid;

pub(crate) use editing::{Replay, Update};

use crate::{
    ByeReason, Event, Frame, LiveConfig, LockTable, MemberId, Mode, Notice, Participant,
    PresenceState,
    frame::LocksSnapshot,
    log::RoomLog,
    member::Member,
    palette,
    protocol::{self, Hello, Limits, MemberView},
};

pub(crate) struct Room {
    members: BTreeMap<MemberId, Member>,
    /// Every change takes the next number; a room starts above every number this node issued
    /// before, so a `seq` never means two things.
    seq: u64,
    leader: Option<MemberId>,
    locks: LockTable,
    log: RoomLog,
    /// The leader was asked to save since the last save; a new leader is asked again.
    save_requested: bool,
    removed: Vec<Member>,
    /// A delivery failed and detached its member; the others have not been told yet.
    pending_announce: bool,
    /// The lock table changed; the members have not been told yet.
    locks_changed: bool,
    /// When the last update was logged.
    last_update: Option<Instant>,
    /// Keys `person`: the same account's connections share one, unlinkable to the account or to
    /// any other room.
    person_key: hmac::Key,
}

impl Room {
    /// An empty room whose numbering starts after `base`.
    pub fn new(base: u64) -> Self {
        Self {
            members: BTreeMap::new(),
            seq: base,
            leader: None,
            locks: LockTable::default(),
            log: RoomLog::new(base),
            save_requested: false,
            removed: Vec::new(),
            pending_announce: false,
            locks_changed: false,
            last_update: None,
            person_key: hmac::Key::new(hmac::HMAC_SHA256, &random_salt()),
        }
    }

    pub fn seq(&self) -> u64 {
        self.seq
    }

    pub fn leader(&self) -> Option<MemberId> {
        self.leader
    }

    pub fn len(&self) -> usize {
        self.members.len()
    }

    pub fn is_empty(&self) -> bool {
        self.members.is_empty()
    }

    /// Members who came through the public link.
    pub fn links(&self) -> usize {
        self.members
            .values()
            .filter(|m| matches!(m.participant, Participant::Link))
            .count()
    }

    /// Members removed since the last call, so the hub can release what they held.
    pub fn take_removed(&mut self) -> Vec<Member> {
        std::mem::take(&mut self.removed)
    }

    /// Adds a member under `id`, which must be greater than every id the room held.
    pub fn add(
        &mut self,
        config: &LiveConfig,
        id: MemberId,
        participant: Participant,
        allowed: Mode,
        address: Option<IpAddr>,
        sender: mpsc::Sender<Event>,
    ) {
        let colour = palette::pick(self.members.values().map(|m| m.colour));
        let slot = allowed == Mode::Edit && self.slots_used() < config.max_editors;
        let person = participant.actor_id().map(|actor| self.person_of(actor));
        self.members.insert(
            id,
            Member {
                participant,
                person,
                colour,
                allowed,
                slot,
                state: PresenceState::Active,
                recheck_queued: false,
                address,
                sender,
            },
        );
        self.seq += 1;
        let leader_changed = self.update_leader();
        self.send_hello(config, id);
        if !self.locks.is_empty() {
            let frame = self.locks_frame();
            self.deliver(id, Event::Send(frame));
        }
        self.broadcast_presence(Some(id));
        if leader_changed {
            self.broadcast_leader(Some(id));
            self.ask_new_leader();
        }
        self.settle(config);
    }

    pub fn remove(&mut self, config: &LiveConfig, id: MemberId) {
        if self.detach(id) {
            let before = self.modes();
            self.after_change(config, before);
        }
        self.settle(config);
    }

    /// Ends the member's session with `reason`, then removes it.
    pub fn close(&mut self, config: &LiveConfig, id: MemberId, reason: ByeReason) {
        if let Some(member) = self.members.get(&id) {
            let _ = member.sender.try_send(Event::Close(reason));
        }
        self.remove(config, id);
    }

    /// The member's role now allows `allowed`.
    pub fn set_allowed(&mut self, config: &LiveConfig, id: MemberId, allowed: Mode) {
        let before = self.modes();
        let Some(member) = self.members.get_mut(&id) else {
            return;
        };
        if member.allowed == allowed {
            return;
        }
        member.allowed = allowed;
        if allowed != Mode::Edit {
            member.slot = false;
        }
        self.after_change(config, before);
        self.settle(config);
    }

    pub fn set_state(&mut self, config: &LiveConfig, id: MemberId, state: PresenceState) {
        let Some(member) = self.members.get_mut(&id) else {
            return;
        };
        if member.state == state {
            return;
        }
        member.state = state;
        self.seq += 1;
        self.broadcast_presence(None);
        self.settle(config);
    }

    /// Asks every session to read its member's role again. A burst of changes queues one recheck
    /// per member, so it cannot fill a queue and drop the member.
    pub fn recheck(&mut self, config: &LiveConfig) {
        let mut ids = Vec::new();
        for (id, member) in &mut self.members {
            if !member.recheck_queued {
                member.recheck_queued = true;
                ids.push(*id);
            }
        }
        for id in ids {
            self.deliver(id, Event::Recheck);
        }
        self.settle(config);
    }

    /// The member's session took up its queued recheck; a change from now on queues another.
    pub fn recheck_started(&mut self, id: MemberId) {
        if let Some(member) = self.members.get_mut(&id) {
            member.recheck_queued = false;
        }
    }

    /// Tells the members whose role allows comments (edit or comment, editors waiting for a slot
    /// included) that a thread changed; viewers and link holders never see comments.
    pub fn comments_changed(&mut self, config: &LiveConfig, thread: Option<Uuid>) {
        let frame = Frame::from(protocol::comments_changed(thread));
        let targets: Vec<MemberId> = self
            .members
            .iter()
            .filter(|(_, m)| m.allowed != Mode::View)
            .map(|(id, _)| *id)
            .collect();
        for id in targets {
            self.deliver(id, Event::Send(frame.clone()));
        }
        self.settle(config);
    }

    /// Saves count only from the leader while more than one member edits; one editor (or none)
    /// saves as without a room.
    pub fn may_save(&self, actor_id: &str) -> bool {
        let editors = self
            .members
            .values()
            .filter(|m| m.mode() == Mode::Edit)
            .count();
        if editors <= 1 {
            return true;
        }
        self.leader
            .and_then(|id| self.members.get(&id))
            .and_then(|m| m.participant.actor_id())
            .is_some_and(|leader| leader == actor_id)
    }

    pub fn locks(&self) -> &LockTable {
        &self.locks
    }

    /// Removes the member and what it held, without telling anyone yet.
    fn detach(&mut self, id: MemberId) -> bool {
        match self.members.remove(&id) {
            Some(member) => {
                self.locks_changed |= self.locks.release_member(id);
                self.removed.push(member);
                true
            }
            None => false,
        }
    }

    /// After members left or a role changed: hands free editor slots to waiting members in join
    /// order, tells those whose mode changed, then everyone the new presence and leader.
    fn after_change(&mut self, config: &LiveConfig, before: Vec<(MemberId, Mode, Option<Notice>)>) {
        if self.members.is_empty() {
            self.leader = None;
            return;
        }
        let mut used = self.slots_used();
        for member in self.members.values_mut() {
            if member.allowed == Mode::Edit && !member.slot && used < config.max_editors {
                member.slot = true;
                used += 1;
            }
        }
        // Only editors hold locks: whoever may no longer edit lets go of them at once.
        let not_editing: Vec<MemberId> = self
            .members
            .iter()
            .filter(|(_, m)| m.mode() != Mode::Edit)
            .map(|(id, _)| *id)
            .collect();
        for id in not_editing {
            self.locks_changed |= self.locks.release_member(id);
        }
        self.seq += 1;
        let changed: Vec<(MemberId, Mode, Option<Notice>)> = self
            .modes()
            .into_iter()
            .filter(|now| {
                before
                    .iter()
                    .any(|was| was.0 == now.0 && (was.1, was.2) != (now.1, now.2))
            })
            .collect();
        for (id, mode, notice) in changed {
            let text = protocol::mode(self.seq, mode, notice);
            self.deliver(id, Event::Send(text.into()));
        }
        let leader_changed = self.update_leader();
        self.broadcast_presence(None);
        if leader_changed {
            self.broadcast_leader(None);
            self.ask_new_leader();
        }
    }

    fn modes(&self) -> Vec<(MemberId, Mode, Option<Notice>)> {
        self.members
            .iter()
            .map(|(id, m)| (*id, m.mode(), m.notice()))
            .collect()
    }

    fn slots_used(&self) -> usize {
        self.members.values().filter(|m| m.slot).count()
    }

    /// The most recent editor still present leads; true when that changed.
    fn update_leader(&mut self) -> bool {
        let leader = self
            .members
            .iter()
            .rev()
            .find(|(_, m)| m.mode() == Mode::Edit)
            .map(|(id, _)| *id);
        let changed = leader != self.leader;
        self.leader = leader;
        changed
    }

    fn views(&self, with_names: bool) -> Vec<MemberView<'_>> {
        self.members
            .iter()
            .map(|(id, m)| MemberView {
                id: *id,
                kind: m.participant.kind(),
                name: if with_names {
                    m.participant.name()
                } else {
                    None
                },
                person: if with_names {
                    m.person.as_deref()
                } else {
                    None
                },
                colour: m.colour,
                mode: m.mode(),
                state: m.state,
            })
            .collect()
    }

    fn send_hello(&mut self, config: &LiveConfig, id: MemberId) {
        let Some(member) = self.members.get(&id) else {
            return;
        };
        let views = self.views(sees_names(member));
        let text = protocol::hello(Hello {
            you: id,
            mode: member.mode(),
            notice: member.notice(),
            colour: member.colour,
            seq: self.seq,
            saved_seq: self.log.saved_seq(),
            saved_version: self.log.saved_version(),
            leader: self.leader,
            members: &views,
            limits: Limits {
                max_editors: config.max_editors,
                max_connections: config.max_per_document,
                max_message_bytes: config.max_message_bytes,
                max_region_message_bytes: config.frame_limit(member.allowed),
                messages_per_second: config.messages_per_second,
                ping_secs: config.ping_interval.as_secs(),
            },
        });
        self.deliver(id, Event::Send(text.into()));
    }

    fn broadcast_presence(&mut self, except: Option<MemberId>) {
        let named = Frame::from(protocol::presence(self.seq, &self.views(true)));
        let anonymous = Frame::from(protocol::presence(self.seq, &self.views(false)));
        let targets: Vec<(MemberId, bool)> = self
            .members
            .iter()
            .filter(|(id, _)| Some(**id) != except)
            .map(|(id, m)| (*id, sees_names(m)))
            .collect();
        for (id, names) in targets {
            let text = if names { &named } else { &anonymous };
            self.deliver(id, Event::Send(text.clone()));
        }
    }

    fn broadcast_leader(&mut self, except: Option<MemberId>) {
        let frame = Frame::from(protocol::leader(self.seq, self.leader));
        self.broadcast(&frame, except);
    }

    /// A short opaque key for `actor` in this room: HMAC-SHA256 under the room's random key, 16
    /// bytes in base64url.
    fn person_of(&self, actor: &str) -> String {
        let tag = hmac::sign(&self.person_key, actor.as_bytes());
        URL_SAFE_NO_PAD.encode(&tag.as_ref()[..16])
    }

    /// The lock table as members see it at the current seq; serialized later, outside the hub lock.
    fn locks_frame(&self) -> Frame {
        let entries = self
            .locks
            .entries()
            .filter_map(|(region, holder)| {
                let colour = self.members.get(&holder)?.colour;
                Some((region.clone(), holder, colour))
            })
            .collect();
        Frame::locks(LocksSnapshot::new(self.seq, entries))
    }

    fn broadcast(&mut self, frame: &Frame, except: Option<MemberId>) {
        let targets: Vec<MemberId> = self
            .members
            .keys()
            .copied()
            .filter(|id| Some(*id) != except)
            .collect();
        for id in targets {
            self.deliver(id, Event::Send(frame.clone()));
        }
    }

    /// The leader changed while a save is wanted: the new one is asked.
    fn ask_new_leader(&mut self) {
        if self.save_requested {
            self.request_save();
        }
    }

    fn request_save(&mut self) {
        if let Some(leader) = self.leader {
            self.deliver(leader, Event::Send(protocol::save_request().into()));
        }
    }

    /// Announces members detached by failed deliveries and lock changes; either may cause the
    /// other.
    fn settle(&mut self, config: &LiveConfig) {
        loop {
            if std::mem::take(&mut self.pending_announce) {
                let before = self.modes();
                self.after_change(config, before);
            } else if std::mem::take(&mut self.locks_changed) {
                self.seq += 1;
                let frame = self.locks_frame();
                self.broadcast(&frame, None);
            } else {
                break;
            }
        }
    }

    /// Queues `event`; a member that cannot take it is detached and announced by
    /// [`Self::drop_unreachable`].
    fn deliver(&mut self, id: MemberId, event: Event) {
        let Some(member) = self.members.get(&id) else {
            return;
        };
        match member.sender.try_send(event) {
            Ok(()) => {}
            Err(TrySendError::Full(_) | TrySendError::Closed(_)) => {
                self.detach(id);
                self.pending_announce = true;
            }
        }
    }
}

/// 32 random bytes; from the system source through `uuid`, which cannot fail.
fn random_salt() -> [u8; 32] {
    let mut salt = [0u8; 32];
    salt[..16].copy_from_slice(Uuid::new_v4().as_bytes());
    salt[16..].copy_from_slice(Uuid::new_v4().as_bytes());
    salt
}

/// Link holders are strangers to the room: they see colours and modes, never names.
fn sees_names(member: &Member) -> bool {
    !matches!(member.participant, Participant::Link)
}
