use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

use crate::MemberId;

/// Region locks of one room: who holds which region until when. M3 only keeps the table and
/// clears a member's locks when it leaves; M4 takes and renews them as members edit.
#[derive(Default)]
pub struct LockTable {
    locks: HashMap<String, Lock>,
}

struct Lock {
    holder: MemberId,
    until: Instant,
}

impl LockTable {
    /// Takes or renews `region` for `holder`; false while someone else holds it unexpired.
    pub fn try_acquire(
        &mut self,
        region: &str,
        holder: MemberId,
        ttl: Duration,
        now: Instant,
    ) -> bool {
        if let Some(lock) = self.locks.get(region)
            && lock.holder != holder
            && lock.until > now
        {
            return false;
        }
        self.locks.insert(
            region.to_owned(),
            Lock {
                holder,
                until: now + ttl,
            },
        );
        true
    }

    pub fn release_member(&mut self, holder: MemberId) {
        self.locks.retain(|_, lock| lock.holder != holder);
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

    #[test]
    fn a_region_has_one_holder_until_it_expires_or_leaves() {
        let mut table = LockTable::default();
        let (a, b) = (MemberId::new(1), MemberId::new(2));
        let now = Instant::now();
        let ttl = Duration::from_secs(5);
        assert!(table.try_acquire("p1", a, ttl, now));
        assert!(table.try_acquire("p1", a, ttl, now), "renewal");
        assert!(!table.try_acquire("p1", b, ttl, now));
        assert!(table.try_acquire("p1", b, ttl, now + ttl), "expired");
        assert!(table.try_acquire("p2", a, ttl, now));
        table.release_member(a);
        assert_eq!(table.len(), 1);
        table.release_member(b);
        assert!(table.is_empty());
    }
}
