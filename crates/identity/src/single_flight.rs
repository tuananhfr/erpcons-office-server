use std::{
    collections::HashMap,
    hash::Hash,
    sync::{Arc, Mutex, MutexGuard, PoisonError},
};

use tokio::sync::OnceCell;

/// Concurrent calls for one key share the work of the first; later calls start afresh.
pub(crate) struct SingleFlight<K, V> {
    inflight: Mutex<HashMap<K, Arc<OnceCell<V>>>>,
}

impl<K: Eq + Hash + Clone, V: Clone> SingleFlight<K, V> {
    pub(crate) fn new() -> Self {
        Self {
            inflight: Mutex::new(HashMap::new()),
        }
    }

    /// Runs `work` unless a call for `key` is already running, then shares its value. Should the
    /// running call be cancelled, a waiting one runs its own `work` instead.
    pub(crate) async fn run<F, Fut>(&self, key: K, work: F) -> V
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = V>,
    {
        let cell = self
            .lock()
            .entry(key.clone())
            .or_insert_with(|| Arc::new(OnceCell::new()))
            .clone();
        let leave = Leave {
            flight: self,
            key,
            cell,
        };
        leave.cell.get_or_init(work).await.clone()
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<K, Arc<OnceCell<V>>>> {
        self.inflight.lock().unwrap_or_else(PoisonError::into_inner)
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.lock().len()
    }
}

/// Drops the entry once its value is known or nobody waits on it, also when the call is cancelled.
struct Leave<'a, K: Eq + Hash + Clone, V: Clone> {
    flight: &'a SingleFlight<K, V>,
    key: K,
    cell: Arc<OnceCell<V>>,
}

impl<K: Eq + Hash + Clone, V: Clone> Drop for Leave<'_, K, V> {
    fn drop(&mut self) {
        let mut map = self.flight.lock();
        let Some(entry) = map.get(&self.key) else {
            return;
        };
        // Clones are made under this lock, so the count cannot grow while it is held. Two
        // references, the map's and this one's: no other call waits.
        if Arc::ptr_eq(entry, &self.cell)
            && (self.cell.initialized() || Arc::strong_count(&self.cell) <= 2)
        {
            map.remove(&self.key);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::atomic::{AtomicUsize, Ordering},
        time::Duration,
    };

    use super::*;

    #[tokio::test]
    async fn concurrent_calls_share_one_run() {
        let flight = SingleFlight::<u8, u32>::new();
        let runs = AtomicUsize::new(0);
        let work = || async {
            runs.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(50)).await;
            7
        };
        let (a, b, c) = tokio::join!(
            flight.run(1, work),
            flight.run(1, work),
            flight.run(1, work)
        );
        assert_eq!((a, b, c), (7, 7, 7));
        assert_eq!(runs.load(Ordering::SeqCst), 1);
        assert_eq!(flight.len(), 0, "the entry goes once the value is known");
        assert_eq!(flight.run(1, work).await, 7);
        assert_eq!(runs.load(Ordering::SeqCst), 2, "a later call runs again");
    }

    #[tokio::test]
    async fn a_cancelled_call_leaves_nothing_behind() {
        let flight = SingleFlight::<u8, u32>::new();
        let slow = flight.run(1, || async {
            tokio::time::sleep(Duration::from_secs(10)).await;
            1
        });
        assert!(
            tokio::time::timeout(Duration::from_millis(20), slow)
                .await
                .is_err()
        );
        assert_eq!(flight.len(), 0);
        assert_eq!(flight.run(1, || async { 2 }).await, 2);
    }
}
