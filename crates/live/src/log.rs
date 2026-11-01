use std::collections::{HashMap, VecDeque};

use sha2::{Digest, Sha256};

use crate::Frame;

/// A room's `region.update`s since the last save, already encoded as their members got them, so a
/// client that reconnects can catch up. Holds every update with `seq > floor`; the floor rises when
/// a save makes older ones redundant or when the log outgrows its byte budget.
pub(crate) struct RoomLog {
    entries: VecDeque<Entry>,
    bytes: usize,
    floor: u64,
    saved_seq: u64,
    saved_version: Option<i32>,
    /// Recent updates' `(actor, cid)` (as [`acked_key`]) and the seq they got, so a resend is
    /// acknowledged again instead of applied twice; never shed, oldest forgotten past the cap.
    acked: HashMap<AckedKey, u64>,
    acked_order: VecDeque<AckedKey>,
    /// What the node budget charges for `acked` at its cap, from the first update on.
    reserved: usize,
}

type AckedKey = [u8; 16];

/// Heap bytes one remembered id may take (map slot, queue slot, table overhead), rounded up.
const ACKED_ENTRY_BYTES: usize = 48;

/// What a room's remembered ids may take at `max_acked`.
pub(crate) fn acked_reserve(max_acked: usize) -> usize {
    max_acked.max(1) * ACKED_ENTRY_BYTES
}

/// A fixed-size key for `(actor, cid)`; 128 bits of sha256 never collide in practice.
fn acked_key(actor: &str, cid: &str) -> AckedKey {
    let mut hash = Sha256::new();
    hash.update((actor.len() as u64).to_le_bytes());
    hash.update(actor.as_bytes());
    hash.update(cid.as_bytes());
    let mut key = [0u8; 16];
    key.copy_from_slice(&hash.finalize()[..16]);
    key
}

struct Entry {
    seq: u64,
    text: Frame,
}

/// A replay the log cannot serve: what was saved last, to reload from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Gone {
    pub saved_seq: u64,
    pub saved_version: Option<i32>,
}

impl RoomLog {
    /// An empty log whose document is as saved at `base`.
    pub fn new(base: u64) -> Self {
        Self {
            entries: VecDeque::new(),
            bytes: 0,
            floor: base,
            saved_seq: base,
            saved_version: None,
            acked: HashMap::new(),
            acked_order: VecDeque::new(),
            reserved: 0,
        }
    }

    /// What the node budget charges: the kept updates and, once the room took one, its remembered
    /// ids at their cap.
    pub fn memory(&self) -> usize {
        self.bytes + self.reserved
    }

    /// Whether the remembered ids are already charged.
    pub fn charged(&self) -> bool {
        self.reserved > 0
    }

    /// Drops the oldest updates until at most `target` bytes are charged or none is left; true
    /// when updates went. Remembered ids stay, so a resend is never applied twice.
    pub fn shed(&mut self, target: usize) -> bool {
        let mut dropped = false;
        while self.memory() > target {
            let Some(old) = self.entries.pop_front() else {
                break;
            };
            self.bytes -= old.text.len();
            self.floor = old.seq;
            dropped = true;
        }
        dropped
    }

    /// Forgets the remembered ids of a room whose every update is saved, giving back their reserve;
    /// true when it did. Only for a room idle long enough that no client still resends.
    pub fn release_saved(&mut self) -> bool {
        if self.reserved == 0 || !self.entries.is_empty() || self.short() {
            return false;
        }
        self.acked = HashMap::new();
        self.acked_order = VecDeque::new();
        self.reserved = 0;
        true
    }

    pub fn saved_seq(&self) -> u64 {
        self.saved_seq
    }

    pub fn saved_version(&self) -> Option<i32> {
        self.saved_version
    }

    pub fn acked(&self, actor: &str, cid: &str) -> Option<u64> {
        self.acked.get(&acked_key(actor, cid)).copied()
    }

    /// Appends an update; true when older ones had to go to stay within `max_bytes`.
    pub fn push(
        &mut self,
        seq: u64,
        text: Frame,
        actor: &str,
        cid: &str,
        max_bytes: usize,
        max_acked: usize,
    ) -> bool {
        self.reserved = acked_reserve(max_acked);
        self.bytes += text.len();
        self.entries.push_back(Entry { seq, text });
        let key = acked_key(actor, cid);
        if self.acked.insert(key, seq).is_none() {
            self.acked_order.push_back(key);
        }
        while self.acked_order.len() > max_acked.max(1) {
            if let Some(old) = self.acked_order.pop_front() {
                self.acked.remove(&old);
            }
        }
        let mut dropped = false;
        while self.bytes > max_bytes {
            let Some(old) = self.entries.pop_front() else {
                break;
            };
            self.bytes -= old.text.len();
            self.floor = old.seq;
            dropped = true;
        }
        dropped
    }

    /// The updates after `since`, when the log still holds all of them and `since` belongs to this
    /// room (`since <= last`).
    pub fn since(&self, since: u64, last: u64) -> Result<Vec<Frame>, Gone> {
        if since < self.floor || since > last {
            return Err(self.gone());
        }
        Ok(self
            .entries
            .iter()
            .filter(|e| e.seq > since)
            .map(|e| e.text.clone())
            .collect())
    }

    /// The document was saved with every update up to `seq`; false when `seq` is not newer than
    /// the last save or not a seq of this room.
    pub fn saved(&mut self, seq: u64, version: i32, last: u64) -> bool {
        if seq <= self.saved_seq || seq > last {
            return false;
        }
        self.saved_seq = seq;
        self.saved_version = Some(version);
        while self.entries.front().is_some_and(|e| e.seq <= seq) {
            if let Some(old) = self.entries.pop_front() {
                self.bytes -= old.text.len();
            }
        }
        self.floor = self.floor.max(seq);
        true
    }

    /// Updates after the last save are no longer all here.
    pub fn short(&self) -> bool {
        self.floor > self.saved_seq
    }

    pub fn gone(&self) -> Gone {
        Gone {
            saved_seq: self.saved_seq,
            saved_version: self.saved_version,
        }
    }

    #[cfg(test)]
    pub fn bytes(&self) -> usize {
        self.bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(s: &str) -> Frame {
        Frame::from(s)
    }

    #[test]
    fn replays_what_came_after_and_refuses_what_it_lost() {
        let mut log = RoomLog::new(100);
        assert!(!log.push(101, text("aaaa"), "u", "c1", 10, 10));
        assert!(!log.push(103, text("bbbb"), "u", "c2", 10, 10));
        assert_eq!(log.since(100, 103).expect("all").len(), 2);
        assert_eq!(log.since(101, 103).expect("one").len(), 1);
        assert_eq!(log.since(103, 103).expect("none").len(), 0);
        assert!(log.since(99, 103).is_err(), "before this room");
        assert!(log.since(104, 103).is_err(), "not issued here");

        assert!(
            log.push(104, text("cccc"), "u", "c3", 10, 10),
            "over 10 bytes"
        );
        assert!(log.short());
        assert_eq!(log.bytes(), 8);
        assert_eq!(
            log.since(100, 104).err(),
            Some(Gone {
                saved_seq: 100,
                saved_version: None
            })
        );
        assert_eq!(log.since(101, 104).expect("still whole after 101").len(), 2);
        assert_eq!(log.acked("u", "c1"), Some(101), "acks outlive entries");
    }

    #[test]
    fn a_save_trims_and_moves_the_floor() {
        let mut log = RoomLog::new(10);
        log.push(11, text("a"), "u", "c1", 100, 100);
        log.push(12, text("b"), "u", "c2", 100, 100);
        log.push(13, text("c"), "u", "c3", 100, 100);
        assert!(!log.saved(10, 2, 13), "not newer");
        assert!(!log.saved(14, 2, 13), "not issued");
        assert!(log.saved(12, 2, 13));
        assert_eq!(log.saved_seq(), 12);
        assert_eq!(log.saved_version(), Some(2));
        assert_eq!(log.bytes(), 1);
        assert!(log.since(11, 13).is_err());
        assert_eq!(log.since(12, 13).expect("after the save").len(), 1);
        assert!(!log.short());
    }

    #[test]
    fn only_a_fully_saved_log_gives_back_its_cids() {
        let mut log = RoomLog::new(0);
        assert!(!log.release_saved(), "nothing charged yet");
        log.push(1, text("a"), "u", "c1", 100, 10);
        assert!(!log.release_saved(), "unsaved");
        assert!(log.saved(1, 2, 1));
        assert!(log.release_saved());
        assert_eq!(log.memory(), 0);
        assert_eq!(log.acked("u", "c1"), None);

        log.push(2, text("aaaa"), "u", "c2", 2, 10);
        assert!(log.short());
        assert!(!log.release_saved(), "updates were lost before a save");
    }

    #[test]
    fn remembers_a_bounded_number_of_cids() {
        let mut log = RoomLog::new(0);
        for (n, cid) in ["a", "b", "c"].into_iter().enumerate() {
            log.push(n as u64 + 1, text("x"), "u", cid, 100, 2);
        }
        assert_eq!(log.acked("u", "a"), None);
        assert_eq!(log.acked("u", "b"), Some(2));
        assert_eq!(log.acked("v", "b"), None, "per account");
    }
}
