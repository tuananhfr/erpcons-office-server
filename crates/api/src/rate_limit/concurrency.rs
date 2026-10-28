use std::{
    collections::HashMap,
    sync::{Arc, Mutex, PoisonError},
};

/// At most `max` requests at once per key (actor). A slot is held until its [`Slot`] drops.
pub(crate) struct ConcurrencyLimiter {
    max: usize,
    active: Mutex<HashMap<String, usize>>,
}

/// A held slot; frees itself when dropped, also when the request is cancelled.
pub(crate) struct Slot {
    limiter: Arc<ConcurrencyLimiter>,
    key: String,
}

impl ConcurrencyLimiter {
    pub(crate) fn new(max: usize) -> Self {
        Self {
            max: max.max(1),
            active: Mutex::new(HashMap::new()),
        }
    }

    /// A slot for `key`, or `None` while `max` are taken.
    pub(crate) fn try_acquire(self: &Arc<Self>, key: &str) -> Option<Slot> {
        let mut active = self.active.lock().unwrap_or_else(PoisonError::into_inner);
        let count = active.entry(key.to_owned()).or_insert(0);
        if *count >= self.max {
            return None;
        }
        *count += 1;
        Some(Slot {
            limiter: self.clone(),
            key: key.to_owned(),
        })
    }

    #[cfg(test)]
    fn keys(&self) -> usize {
        self.active
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        let mut active = self
            .limiter
            .active
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(count) = active.get_mut(&self.key) {
            *count = count.saturating_sub(1);
            // Idle keys are dropped, so the map only holds actors with a request running.
            if *count == 0 {
                active.remove(&self.key);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slots_are_counted_per_key_and_freed_on_drop() {
        let limiter = Arc::new(ConcurrencyLimiter::new(2));
        let a1 = limiter.try_acquire("a").expect("first");
        let a2 = limiter.try_acquire("a").expect("second");
        assert!(limiter.try_acquire("a").is_none(), "third is refused");
        let b = limiter
            .try_acquire("b")
            .expect("another key has its own slots");
        drop(a1);
        let a3 = limiter.try_acquire("a").expect("a freed slot is reusable");
        drop((a2, a3, b));
        assert_eq!(limiter.keys(), 0, "idle keys are forgotten");
    }

    #[test]
    fn a_refused_acquire_changes_nothing() {
        let limiter = Arc::new(ConcurrencyLimiter::new(1));
        let held = limiter.try_acquire("a").expect("slot");
        assert!(limiter.try_acquire("a").is_none());
        drop(held);
        assert_eq!(limiter.keys(), 0);
    }
}
