//! Co-editing in a room (plan-m4): region locks, `region.update` with its seq and `ack`, cursors,
//! replay of the log, and saves by the leader. Answers go through the member's queue like
//! everything else, so a client sees them in seq order with the others' updates.
use std::{sync::Arc, time::Instant};

use tokio::sync::mpsc::error::TrySendError;

use super::Room;
use crate::{
    Event, LiveConfig, MemberId, Mode,
    locks::Refusal,
    protocol::{self, LiveErrorCode},
};

/// What a `region.replay` gets, sent straight to the socket: the queue could not hold a whole log.
pub(crate) enum Replay {
    /// The logged updates after `since`, then `region.replayed`.
    Updates {
        updates: Vec<Arc<str>>,
        done: Arc<str>,
    },
    /// `error` `REPLAY_GONE`.
    Gone(Arc<str>),
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

    /// Logs and relays an update of a region the sender holds; a resend of a `cid` already taken
    /// gets the same `ack` again and changes nothing.
    pub fn update(
        &mut self,
        config: &LiveConfig,
        id: MemberId,
        region: &str,
        cid: &str,
        payload: &str,
        now: Instant,
    ) {
        if !self.may_edit(id, Some(cid)) {
            return self.settle(config);
        }
        // Only accounts edit, so an editor always has one; idempotency must outlive the member id,
        // which a reconnect changes.
        let Some(actor) = self
            .members
            .get(&id)
            .and_then(|m| m.participant.actor_id())
            .map(str::to_owned)
        else {
            return self.settle(config);
        };
        if let Some(seq) = self.log.acked(&actor, cid) {
            self.deliver(id, Event::Send(protocol::ack(cid, seq).into()));
            return self.settle(config);
        }
        if !self.locks.touch(region, id, config.lock_ttl, now) {
            let text = protocol::error_for(LiveErrorCode::LockRequired, cid);
            self.deliver(id, Event::Send(text.into()));
            return self.settle(config);
        }
        self.seq += 1;
        let seq = self.seq;
        let text: Arc<str> = protocol::region_update(seq, region, id, payload).into();
        let overflowed = self.log.push(
            seq,
            Arc::clone(&text),
            &actor,
            cid,
            config.max_log_bytes,
            config.max_acked_cids,
        );
        self.deliver(id, Event::Send(protocol::ack(cid, seq).into()));
        self.broadcast(&text, Some(id));
        if overflowed && !self.save_requested {
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
        let text: Arc<str> = protocol::cursor(id, region, range).into();
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
    pub fn replay(&self, id: MemberId, since: u64) -> Option<Replay> {
        if !self.members.contains_key(&id) {
            return None;
        }
        Some(match self.log.since(since, self.seq) {
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
        let text: Arc<str> = protocol::saved(self.log.saved_seq(), version).into();
        self.broadcast(&text, None);
        // A save older than the trimmed log leaves newcomers unable to replay: ask again.
        self.save_requested = self.log.short();
        if self.save_requested {
            self.request_save();
        }
        self.settle(config);
    }

    /// Estimated heap bytes of the room's log, for the node's budget.
    pub fn log_memory(&self) -> usize {
        self.log.memory()
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
