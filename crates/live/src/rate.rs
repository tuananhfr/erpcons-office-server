use std::time::Instant;

/// Token bucket of one connection's messages.
pub struct MessageRate {
    burst: f64,
    per_second: f64,
    tokens: f64,
    updated: Instant,
}

impl MessageRate {
    pub fn new(burst: u32, per_second: f64, now: Instant) -> Self {
        let burst = f64::from(burst.max(1));
        Self {
            burst,
            per_second,
            tokens: burst,
            updated: now,
        }
    }

    /// Takes a token; false when none is left.
    pub fn check(&mut self, now: Instant) -> bool {
        self.take(now, 1.0)
    }

    /// Takes `cost` tokens (bytes, say); false, taking nothing, when fewer are left.
    pub fn take(&mut self, now: Instant, cost: f64) -> bool {
        let elapsed = now.saturating_duration_since(self.updated).as_secs_f64();
        self.tokens = (self.tokens + elapsed * self.per_second).min(self.burst);
        self.updated = now;
        if self.tokens >= cost {
            self.tokens -= cost;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn allows_the_burst_then_the_rate() {
        let now = Instant::now();
        let mut rate = MessageRate::new(3, 2.0, now);
        assert!((0..3).all(|_| rate.check(now)));
        assert!(!rate.check(now));
        assert!(rate.check(now + Duration::from_millis(500)));
        assert!(!rate.check(now + Duration::from_millis(500)));
    }

    #[test]
    fn takes_a_cost_or_nothing() {
        let now = Instant::now();
        let mut bytes = MessageRate::new(100, 10.0, now);
        assert!(bytes.take(now, 60.0));
        assert!(!bytes.take(now, 60.0), "only 40 left");
        assert!(bytes.take(now, 40.0));
        assert!(bytes.take(now + Duration::from_secs(2), 20.0));
    }
}
