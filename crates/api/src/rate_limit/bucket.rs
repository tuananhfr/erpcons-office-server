use std::{
    collections::HashMap,
    hash::Hash,
    sync::{Mutex, PoisonError},
    time::Instant,
};

use crate::RateConfig;

const MAX_KEYS: usize = 100_000;
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
            // Still full: forget everything rather than let memory grow without bound.
            if buckets.len() >= self.max_keys {
                buckets.clear();
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
