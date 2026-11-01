//! One document's room: members in join order, the change sequence, editor slots, the leader and
//! the lock table. Every change sends what it changed to the members it concerns; a member whose
//! queue is full or whose session is gone is removed, and [`Room::take_removed`] hands it back.
use std::{collections::BTreeMap, net::IpAddr};

use tokio::sync::mpsc::{self, error::TrySendError};

use crate::{
    ByeReason, Event, LiveConfig, LockTable, MemberId, Mode, Notice, Participant, PresenceState,
    member::Member,
    palette,
    protocol::{self, Hello, Limits, MemberView},
};

pub(crate) struct Room {
    members: BTreeMap<MemberId, Member>,
    next_id: u64,
    seq: u64,
    leader: Option<MemberId>,
    locks: LockTable,
    removed: Vec<Member>,
    /// A delivery failed and detached its member; the others have not been told yet.
    pending_announce: bool,
}

impl Room {
    pub fn new() -> Self {
        Self {
            members: BTreeMap::new(),
            next_id: 1,
            seq: 0,
            leader: None,
            locks: LockTable::default(),
            removed: Vec::new(),
            pending_announce: false,
        }
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

    pub fn add(
        &mut self,
        config: &LiveConfig,
        participant: Participant,
        allowed: Mode,
        address: Option<IpAddr>,
        sender: mpsc::Sender<Event>,
    ) -> MemberId {
        let id = MemberId::new(self.next_id);
        self.next_id += 1;
        let colour = palette::pick(self.members.values().map(|m| m.colour));
        let slot = allowed == Mode::Edit && self.slots_used() < config.max_editors;
        self.members.insert(
            id,
            Member {
                participant,
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
        self.broadcast_presence(Some(id));
        if leader_changed {
            self.broadcast_leader(Some(id));
        }
        self.drop_unreachable(config);
        id
    }

    pub fn remove(&mut self, config: &LiveConfig, id: MemberId) {
        if self.detach(id) {
            let before = self.modes();
            self.after_change(config, before);
        }
        self.drop_unreachable(config);
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
        self.drop_unreachable(config);
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
        self.drop_unreachable(config);
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
        self.drop_unreachable(config);
    }

    /// The member's session took up its queued recheck; a change from now on queues another.
    pub fn recheck_started(&mut self, id: MemberId) {
        if let Some(member) = self.members.get_mut(&id) {
            member.recheck_queued = false;
        }
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

    pub fn locks_mut(&mut self) -> &mut LockTable {
        &mut self.locks
    }

    /// Removes the member and what it held, without telling anyone yet.
    fn detach(&mut self, id: MemberId) -> bool {
        match self.members.remove(&id) {
            Some(member) => {
                self.locks.release_member(id);
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
            self.deliver(id, Event::Send(protocol::mode(self.seq, mode, notice)));
        }
        let leader_changed = self.update_leader();
        self.broadcast_presence(None);
        if leader_changed {
            self.broadcast_leader(None);
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
            leader: self.leader,
            members: &views,
            limits: Limits {
                max_message_bytes: config.max_message_bytes,
                messages_per_second: config.messages_per_second,
                ping_secs: config.ping_interval.as_secs(),
            },
        });
        self.deliver(id, Event::Send(text));
    }

    fn broadcast_presence(&mut self, except: Option<MemberId>) {
        let named = protocol::presence(self.seq, &self.views(true));
        let anonymous = protocol::presence(self.seq, &self.views(false));
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
        let text = protocol::leader(self.seq, self.leader);
        let targets: Vec<MemberId> = self
            .members
            .keys()
            .copied()
            .filter(|id| Some(*id) != except)
            .collect();
        for id in targets {
            self.deliver(id, Event::Send(text.clone()));
        }
    }

    /// Announces members detached by failed deliveries; announcing may detach more.
    fn drop_unreachable(&mut self, config: &LiveConfig) {
        while std::mem::take(&mut self.pending_announce) {
            let before = self.modes();
            self.after_change(config, before);
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

/// Link holders are strangers to the room: they see colours and modes, never names.
fn sees_names(member: &Member) -> bool {
    !matches!(member.participant, Participant::Link)
}
