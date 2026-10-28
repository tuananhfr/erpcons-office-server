use std::{
    collections::HashMap,
    hash::Hash,
    sync::{Mutex, PoisonError},
    time::Instant,
};

use crate::RateConfig;

const MAX_KEYS: usize = 100_000;
/// Share of the keys dropped when the table is full of busy buckets: evicting in batches keeps
/// the cost of a full table spread over many requests.
const EVICT_DIVISOR: usize = 10;
/// `Retry-After` when the configured rate cannot refill at all.
const NO_REFILL_RETRY_SECS: u64 = 60;

struct Bucket {
    tokens: f64,
    updated: Instant,
}

/// One token bucket per key (client address or actor).
pub(crate) struct KeyedLimiter<K> {
    config: RateConfig,
    buckets: Mutex<HashMap<K, Bucket>>,
    max_keys: usize,
}

impl<K: Eq + Hash> KeyedLimiter<K> {
    pub(crate) fn new(config: RateConfig) -> Self {
        Self::with_max_keys(config, MAX_KEYS)
    }

    fn with_max_keys(config: RateConfig, max_keys: usize) -> Self {
        Self {
            config,
            buckets: Mutex::new(HashMap::new()),
            max_keys,
        }
    }

    /// Takes a token for `key`; when none is left, the whole seconds until one is (`Retry-After`).
    pub(crate) fn check(&self, key: K, now: Instant) -> Result<(), u64> {
        let burst = f64::from(self.config.burst);
        let rate = self.config.per_second;
        let mut buckets = self.buckets.lock().unwrap_or_else(PoisonError::into_inner);
        if buckets.len() >= self.max_keys && !buckets.contains_key(&key) {
            // A bucket that refilled completely holds no state worth keeping.
            buckets.retain(|_, b| refilled(b, now, rate, burst) < burst);
            // Still full: forget the least recently used, so a flood of new keys cannot reset
            // the buckets of the clients that are being limited.
            if buckets.len() >= self.max_keys {
                let count = (buckets.len() / EVICT_DIVISOR).max(1);
                evict_oldest(&mut buckets, count);
            }
        }
        let bucket = buckets.entry(key).or_insert(Bucket {
            tokens: burst,
            updated: now,
        });
        bucket.tokens = refilled(bucket, now, rate, burst);
        bucket.updated = now;
        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            return Ok(());
        }
        if rate <= 0.0 {
            return Err(NO_REFILL_RETRY_SECS);
        }
        Err(((1.0 - bucket.tokens) / rate).ceil().max(1.0) as u64)
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.buckets
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }
}

/// Removes the `count` buckets used longest ago (ties may take a few more).
fn evict_oldest<K>(buckets: &mut HashMap<K, Bucket>, count: usize) {
    let mut times: Vec<Instant> = buckets.values().map(|b| b.updated).collect();
    if count == 0 || times.is_empty() {
        return;
    }
    let nth = count.min(times.len()) - 1;
    let (_, cutoff, _) = times.select_nth_unstable(nth);
    let cutoff = *cutoff;
    buckets.retain(|_, b| b.updated > cutoff);
}

fn refilled(bucket: &Bucket, now: Instant, rate: f64, burst: f64) -> f64 {
    let elapsed = now.saturating_duration_since(bucket.updated).as_secs_f64();
    (bucket.tokens + elapsed * rate.max(0.0)).min(burst)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn limiter(burst: u32, per_second: f64) -> KeyedLimiter<&'static str> {
        KeyedLimiter::new(RateConfig { burst, per_second })
    }

    #[test]
    fn allows_a_burst_then_refuses_with_retry_after() {
        let l = limiter(3, 0.5);
        let now = Instant::now();
        for _ in 0..3 {
            assert_eq!(l.check("a", now), Ok(()));
        }
        assert_eq!(l.check("a", now), Err(2));
    }

    #[test]
    fn refills_over_time() {
        let l = limiter(1, 2.0);
        let now = Instant::now();
        assert_eq!(l.check("a", now), Ok(()));
        assert_eq!(l.check("a", now), Err(1));
        assert_eq!(l.check("a", now + Duration::from_millis(500)), Ok(()));
    }

    #[test]
    fn keys_are_independent() {
        let l = limiter(1, 0.1);
        let now = Instant::now();
        assert_eq!(l.check("a", now), Ok(()));
        assert!(l.check("a", now).is_err());
        assert_eq!(l.check("b", now), Ok(()));
    }

    #[test]
    fn zero_rate_never_refills() {
        let l = limiter(1, 0.0);
        let now = Instant::now();
        assert_eq!(l.check("a", now), Ok(()));
        let later = now + Duration::from_secs(3600);
        assert_eq!(l.check("a", later), Err(NO_REFILL_RETRY_SECS));
    }

    #[test]
    fn a_full_table_of_busy_buckets_evicts_the_least_recently_used() {
        let l = KeyedLimiter::with_max_keys(
            RateConfig {
                burst: 1,
                per_second: 0.001,
            },
            3,
        );
        let t = Instant::now();
        l.check("a", t).expect("a");
        l.check("b", t + Duration::from_secs(1)).expect("b");
        l.check("c", t + Duration::from_secs(2)).expect("c");
        l.check("d", t + Duration::from_secs(3)).expect("d");
        assert_eq!(l.len(), 3);
        let later = t + Duration::from_secs(4);
        assert!(l.check("b", later).is_err(), "b keeps its empty bucket");
        assert!(l.check("c", later).is_err(), "c too");
        assert_eq!(l.check("a", later), Ok(()), "a, the oldest, was evicted");
    }

    #[test]
    fn idle_buckets_are_dropped_when_full() {
        let l = KeyedLimiter::with_max_keys(
            RateConfig {
                burst: 1,
                per_second: 1.0,
            },
            2,
        );
        let now = Instant::now();
        l.check("a", now).expect("a");
        l.check("b", now).expect("b");
        let later = now + Duration::from_secs(5);
        l.check("c", later).expect("c");
        assert_eq!(l.len(), 1);
    }
}
