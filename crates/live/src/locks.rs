use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

use crate::{MemberId, region};

/// Region locks of one room: who holds which region until when. A lock lapses unless renewed
/// within its lease; a lapsed lock refuses nobody even before [`LockTable::expire`] removes it.
#[derive(Default)]
pub struct LockTable {
    locks: BTreeMap<String, Lock>,
}

struct Lock {
    holder: MemberId,
    until: Instant,
}

/// Why a lock was not granted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// Another member holds a clashing lock; the one that lapses last.
    Held {
        holder: MemberId,
        remaining: Duration,
    },
    /// The member holds as many locks as it may.
    Limit,
}

impl LockTable {
    /// Takes or renews `region` for `holder` for `ttl`. `Ok(true)` when the table changed (a new
    /// lock, not a renewal).
    pub fn acquire(
        &mut self,
        region: &str,
        holder: MemberId,
        ttl: Duration,
        max_per_member: usize,
        now: Instant,
    ) -> Result<bool, Refusal> {
        if let Some(clash) = self
            .locks
            .iter()
            .filter(|(r, l)| l.holder != holder && l.until > now && region::conflicts(region, r))
            .map(|(_, l)| l)
            .max_by_key(|l| l.until)
        {
            return Err(Refusal::Held {
                holder: clash.holder,
                remaining: clash.until - now,
            });
        }
        let renewal = self
            .locks
            .get(region)
            .is_some_and(|l| l.holder == holder && l.until > now);
        if !renewal && self.held_by(holder, now) >= max_per_member {
            return Err(Refusal::Limit);
        }
        self.locks.insert(
            region.to_owned(),
            Lock {
                holder,
                until: now + ttl,
            },
        );
        Ok(!renewal)
    }

    /// Releases `region` if `holder` has it; true when it did.
    pub fn release(&mut self, region: &str, holder: MemberId) -> bool {
        if self.locks.get(region).is_some_and(|l| l.holder == holder) {
            self.locks.remove(region);
            true
        } else {
            false
        }
    }

    /// Releases everything `holder` has; true when it had anything.
    pub fn release_member(&mut self, holder: MemberId) -> bool {
        let before = self.locks.len();
        self.locks.retain(|_, lock| lock.holder != holder);
        self.locks.len() != before
    }

    /// Whether `holder` has an unlapsed lock covering `region` (itself, `*`, or its sheet); each
    /// such lock is extended by `ttl`, as editing counts as renewing.
    pub fn touch(&mut self, region: &str, holder: MemberId, ttl: Duration, now: Instant) -> bool {
        let mut covered = false;
        for (r, lock) in &mut self.locks {
            if lock.holder == holder && lock.until > now && region::covers(r, region) {
                lock.until = now + ttl;
                covered = true;
            }
        }
        covered
    }

    /// Removes lapsed locks; true when any was.
    pub fn expire(&mut self, now: Instant) -> bool {
        let before = self.locks.len();
        self.locks.retain(|_, lock| lock.until > now);
        self.locks.len() != before
    }

    /// Region and holder of every lock, in region order.
    pub fn entries(&self) -> impl Iterator<Item = (&str, MemberId)> {
        self.locks.iter().map(|(r, l)| (r.as_str(), l.holder))
    }

    fn held_by(&self, holder: MemberId, now: Instant) -> usize {
        self.locks
            .values()
            .filter(|l| l.holder == holder && l.until > now)
            .count()
    }

    pub fn len(&self) -> usize {
        self.locks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.locks.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TTL: Duration = Duration::from_secs(5);
    const MAX: usize = 8;

    fn ids() -> (MemberId, MemberId) {
        (MemberId::new(1), MemberId::new(2))
    }

    #[test]
    fn a_region_has_one_holder_until_it_lapses_or_is_released() {
        let mut table = LockTable::default();
        let (a, b) = ids();
        let now = Instant::now();
        assert_eq!(table.acquire("p1", a, TTL, MAX, now), Ok(true));
        assert_eq!(table.acquire("p1", a, TTL, MAX, now), Ok(false), "renewal");
        assert_eq!(
            table.acquire("p1", b, TTL, MAX, now + Duration::from_secs(1)),
            Err(Refusal::Held {
                holder: a,
                remaining: Duration::from_secs(4)
            })
        );
        assert_eq!(
            table.acquire("p1", b, TTL, MAX, now + TTL),
            Ok(true),
            "lapsed"
        );
        assert!(!table.release("p1", a), "not a's any more");
        assert!(table.release("p1", b));
        assert_eq!(table.acquire("p2", a, TTL, MAX, now), Ok(true));
        assert!(table.release_member(a));
        assert!(!table.release_member(a));
        assert!(table.is_empty());
    }

    #[test]
    fn the_whole_document_excludes_others_and_is_excluded_by_them() {
        let mut table = LockTable::default();
        let (a, b) = ids();
        let now = Instant::now();
        table.acquire("para:1", a, TTL, MAX, now).expect("a para");
        assert!(matches!(
            table.acquire("*", b, TTL, MAX, now),
            Err(Refusal::Held { holder, .. }) if holder == a
        ));
        assert_eq!(
            table.acquire("*", a, TTL, MAX, now),
            Ok(true),
            "its own locks do not stop a member"
        );
        assert!(table.acquire("para:2", b, TTL, MAX, now).is_err());
        table.release_member(a);
        assert_eq!(table.acquire("*", b, TTL, MAX, now), Ok(true));
    }

    #[test]
    fn a_sheet_lock_excludes_the_cells_of_that_sheet() {
        let mut table = LockTable::default();
        let (a, b) = ids();
        let now = Instant::now();
        table.acquire("cell:s1:A1", a, TTL, MAX, now).expect("cell");
        assert!(table.acquire("sheet:s1", b, TTL, MAX, now).is_err());
        assert_eq!(table.acquire("sheet:s2", b, TTL, MAX, now), Ok(true));
        assert!(table.acquire("cell:s2:A1", a, TTL, MAX, now).is_err());
        assert_eq!(table.acquire("cell:s1:A2", a, TTL, MAX, now), Ok(true));
    }

    #[test]
    fn touching_needs_a_covering_lock_and_extends_it() {
        let mut table = LockTable::default();
        let (a, b) = ids();
        let now = Instant::now();
        table.acquire("sheet:s1", a, TTL, MAX, now).expect("sheet");
        let later = now + Duration::from_secs(4);
        assert!(table.touch("cell:s1:C3", a, TTL, later));
        assert!(!table.touch("cell:s2:C3", a, TTL, later));
        assert!(!table.touch("cell:s1:C3", b, TTL, later));
        let after_first_lease = now + Duration::from_secs(6);
        assert!(!table.expire(after_first_lease), "touch extended it");
        assert!(table.touch("sheet:s1", a, TTL, after_first_lease));
        assert!(table.expire(after_first_lease + TTL));
        assert!(!table.touch("sheet:s1", a, TTL, after_first_lease + TTL));
    }

    #[test]
    fn a_member_holds_a_limited_number_of_locks() {
        let mut table = LockTable::default();
        let (a, _) = ids();
        let now = Instant::now();
        table.acquire("r1", a, TTL, 2, now).expect("r1");
        table.acquire("r2", a, TTL, 2, now).expect("r2");
        assert_eq!(table.acquire("r3", a, TTL, 2, now), Err(Refusal::Limit));
        assert_eq!(table.acquire("r2", a, TTL, 2, now), Ok(false), "renewal");
        assert_eq!(
            table.acquire("r3", a, TTL, 2, now + TTL),
            Ok(true),
            "lapsed ones do not count"
        );
    }
}
