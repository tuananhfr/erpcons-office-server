//! Co-editing in a room (plan-m4): region locks, `region.update` with its seq and `ack`, cursors,
//! replay of the log, and saves by the leader. Answers go through the member's queue like
//! everything else, so a client sees them in seq order with the others' updates.
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use tokio::sync::mpsc::error::TrySendError;

use super::Room;
use crate::{
    Event, Frame, LiveConfig, MemberId, Mode,
    locks::Refusal,
    log::{Origin, acked_reserve},
    protocol::{self, LiveErrorCode},
};

/// A client's `region.update` that passed [`Room::check_update`]; `head` is the message without
/// its seq ([`protocol::region_update_head`]).
pub(crate) struct Update<'a> {
    pub actor: &'a str,
    pub cid: &'a str,
    pub head: Arc<str>,
}

/// What a `region.replay` gets, sent straight to the socket: the queue could not hold a whole log.
pub(crate) enum Replay {
    /// The logged updates after `since`, then `region.replayed`.
    Updates { updates: Vec<Frame>, done: Frame },
    /// `error` `REPLAY_GONE`.
    Gone(Frame),
}

impl Room {
    pub fn acquire_lock(
        &mut self,
        config: &LiveConfig,
        id: MemberId,
        region: &str,
        cid: &str,
        now: Instant,
    ) {
        if !self.may_edit(id, Some(cid)) {
            return self.settle(config);
        }
        let ttl = config.lock_ttl;
        let reply = match self
            .locks
            .acquire(region, id, ttl, config.max_locks_per_member, now)
        {
            Ok(changed) => {
                self.locks_changed |= changed;
                protocol::lock_granted(region, cid, id, ttl.as_millis())
            }
            Err(Refusal::Held { holder, remaining }) => {
                protocol::lock_denied(region, Some(cid), holder, remaining.as_millis())
            }
            Err(Refusal::Limit) => protocol::error_for(LiveErrorCode::LockLimit, cid),
        };
        self.deliver(id, Event::Send(reply.into()));
        self.settle(config);
    }

    /// Extends the lease; a lapsed lock nobody else took is taken again. Silent unless refused.
    pub fn renew_lock(&mut self, config: &LiveConfig, id: MemberId, region: &str, now: Instant) {
        if !self.may_edit(id, None) {
            return self.settle(config);
        }
        let refused = match self.locks.acquire(
            region,
            id,
            config.lock_ttl,
            config.max_locks_per_member,
            now,
        ) {
            Ok(changed) => {
                self.locks_changed |= changed;
                None
            }
            Err(Refusal::Held { holder, remaining }) => Some(protocol::lock_denied(
                region,
                None,
                holder,
                remaining.as_millis(),
            )),
            Err(Refusal::Limit) => Some(protocol::error(LiveErrorCode::LockLimit)),
        };
        if let Some(text) = refused {
            self.deliver(id, Event::Send(text.into()));
        }
        self.settle(config);
    }

    pub fn release_lock(&mut self, config: &LiveConfig, id: MemberId, region: &str) {
        self.locks_changed |= self.locks.release(region, id);
        self.settle(config);
    }

    /// Locks whose lease ran out are gone; members hear of it.
    pub fn expire_locks(&mut self, config: &LiveConfig, now: Instant) {
        self.locks_changed |= self.locks.expire(now);
        self.settle(config);
    }

    /// Whether the member may log an update of `region` now; returns its account if so. Otherwise
    /// it has had its answer: `FORBIDDEN_MODE`, `REPLAY_GONE` (no successful replay on this
    /// connection yet, or since one the room forgot a cid it may resend), `LOCK_REQUIRED`, or for
    /// a resend of a `cid` already taken the same `ack` again. Nothing here touches the node's log
    /// budget, so a refused update never makes another room shed.
    pub fn check_update(
        &mut self,
        config: &LiveConfig,
        id: MemberId,
        region: &str,
        cid: &str,
        now: Instant,
    ) -> Option<String> {
        if !self.may_edit(id, Some(cid)) {
            self.settle(config);
            return None;
        }
        // Only accounts edit, so an editor always has one; idempotency must outlive the member id,
        // which a reconnect changes.
        let Some(member) = self.members.get(&id) else {
            self.settle(config);
            return None;
        };
        if member.synced.is_none() {
            let gone = self.log.gone();
            let text = protocol::replay_gone_for(cid, gone.saved_seq, gone.saved_version);
            self.deliver(id, Event::Send(text.into()));
            self.settle(config);
            return None;
        }
        let Some(actor) = member.participant.actor_id().map(str::to_owned) else {
            self.settle(config);
            return None;
        };
        if let Some(seq) = self.log.acked(&actor, cid) {
            self.deliver(id, Event::Send(protocol::ack(cid, seq).into()));
            self.settle(config);
            return None;
        }
        if !self.locks.touch(region, id, config.lock_ttl, now) {
            let text = protocol::error_for(LiveErrorCode::LockRequired, cid);
            self.deliver(id, Event::Send(text.into()));
            self.settle(config);
            return None;
        }
        Some(actor)
    }

    /// Logs and relays an update [`Self::check_update`] let through.
    /// `room_left`: whether the node's log budget takes it; if not it is refused `SERVER_BUSY`
    /// (with `cid`) for the client to send again later.
    pub fn apply_update(
        &mut self,
        config: &LiveConfig,
        id: MemberId,
        update: Update<'_>,
        room_left: bool,
        now: Instant,
    ) {
        let Update { actor, cid, head } = update;
        // Making room on the node may have dropped the member (a full queue).
        if !self.members.contains_key(&id) {
            return self.settle(config);
        }
        if !room_left {
            let text = protocol::error_for(LiveErrorCode::ServerBusy, cid);
            self.deliver(id, Event::Send(text.into()));
            return self.settle(config);
        }
        self.seq += 1;
        let seq = self.seq;
        let frame = Frame::split(head, protocol::region_update_tail(seq));
        let origin = Origin {
            actor,
            member: id,
            cid,
        };
        let pushed = self.log.push(
            seq,
            frame.clone(),
            origin,
            config.max_log_bytes,
            config.max_acked_cids,
        );
        self.forget(&pushed.forgot);
        // Replays from the last save now answer REPLAY_GONE until a newer save.
        let forgot_unsaved = self.forgotten > self.log.saved_seq();
        self.last_update = Some(now);
        self.deliver(id, Event::Send(protocol::ack(cid, seq).into()));
        self.broadcast(&frame, Some(id));
        if (pushed.overflowed || forgot_unsaved) && !self.save_requested {
            self.save_requested = true;
            self.request_save();
        }
        self.settle(config);
    }

    /// Relays a cursor to everyone else, best effort: a member whose queue is half full misses it,
    /// so cursors never crowd out what must arrive. Never logged.
    pub fn cursor(&mut self, config: &LiveConfig, id: MemberId, region: &str, range: &str) {
        let Some(member) = self.members.get(&id) else {
            return;
        };
        if member.mode() == Mode::View {
            let text = protocol::error(LiveErrorCode::ForbiddenMode);
            self.deliver(id, Event::Send(text.into()));
            return self.settle(config);
        }
        let text = Frame::from(protocol::cursor(id, region, range));
        let mut gone = Vec::new();
        for (other, member) in &self.members {
            if *other == id || member.sender.capacity() <= config.outbox / 2 {
                continue;
            }
            if let Err(TrySendError::Closed(_)) = member.sender.try_send(Event::Send(text.clone()))
            {
                gone.push(*other);
            }
        }
        for other in gone {
            self.detach(other);
            self.pending_announce = true;
        }
        self.settle(config);
    }

    /// The logged updates after `since`, or why they cannot be had; `None` for a stale member.
    /// Only a replay the room can fully answer lets the member update: one from before a cid the
    /// room forgot could be followed by that cid, resent.
    pub fn replay(&mut self, id: MemberId, since: u64) -> Option<Replay> {
        let answer = if since < self.forgotten {
            Err(self.log.gone())
        } else {
            self.log.since(since, self.seq)
        };
        let member = self.members.get_mut(&id)?;
        member.synced = answer.is_ok().then_some(since);
        Some(match answer {
            Ok(updates) => Replay::Updates {
                updates,
                done: protocol::region_replayed(since, self.seq).into(),
            },
            Err(gone) => {
                Replay::Gone(protocol::replay_gone(gone.saved_seq, gone.saved_version).into())
            }
        })
    }

    /// `actor_id` saved version `version` holding every update up to `live_seq`. Counts only from
    /// the leader's account and for a seq of this room newer than the last save: the log is
    /// trimmed and everyone hears `saved`.
    pub fn saved(&mut self, config: &LiveConfig, actor_id: &str, live_seq: u64, version: i32) {
        let leads = self
            .leader
            .and_then(|id| self.members.get(&id))
            .and_then(|m| m.participant.actor_id())
            .is_some_and(|leader| leader == actor_id);
        if !leads || !self.log.saved(live_seq, version, self.seq) {
            return;
        }
        let frame = Frame::from(protocol::saved(self.log.saved_seq(), version));
        self.broadcast(&frame, None);
        // A save older than the trimmed log or a forgotten cid leaves newcomers unable to replay:
        // ask again.
        self.save_requested = self.log.short() || self.forgotten > self.log.saved_seq();
        if self.save_requested {
            self.request_save();
        }
        self.settle(config);
    }

    /// What the room's log is charged on the node's budget.
    pub fn log_memory(&self) -> usize {
        self.log.memory()
    }

    /// What one more update of `bytes` adds to the node's budget, the remembered ids' reserve
    /// included on the room's first.
    pub fn log_cost(&self, config: &LiveConfig, bytes: usize) -> usize {
        if self.log.charged() {
            bytes
        } else {
            bytes + acked_reserve(config.max_acked_cids)
        }
    }

    /// Gives back the remembered ids' reserve of a room with nothing unsaved and no update for
    /// `idle`; true when it did.
    pub fn release_idle(&mut self, now: Instant, idle: Duration) -> bool {
        let quiet = self
            .last_update
            .is_some_and(|last| now.saturating_duration_since(last) >= idle);
        if !quiet {
            return false;
        }
        let Some(forgot) = self.log.release_saved() else {
            return false;
        };
        self.forget(&forgot);
        true
    }

    /// Sheds the log to at most `target` bytes for the node's budget; the leader is asked to save
    /// when updates went, since newcomers can no longer replay them.
    pub fn shed_log(&mut self, config: &LiveConfig, target: usize) {
        if self.log.shed(target) && !self.save_requested {
            self.save_requested = true;
            self.request_save();
        }
        self.settle(config);
    }

    /// Whether the member may edit now; if not it is told `FORBIDDEN_MODE` (with `cid`, if any).
    fn may_edit(&mut self, id: MemberId, cid: Option<&str>) -> bool {
        let Some(member) = self.members.get(&id) else {
            return false;
        };
        if member.mode() == Mode::Edit {
            return true;
        }
        let text = match cid {
            Some(cid) => protocol::error_for(LiveErrorCode::ForbiddenMode, cid),
            None => protocol::error(LiveErrorCode::ForbiddenMode),
        };
        self.deliver(id, Event::Send(text.into()));
        false
    }
}
