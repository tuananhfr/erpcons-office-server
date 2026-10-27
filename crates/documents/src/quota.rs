const MIB: u64 = 1024 * 1024;

/// Storage limits per actor (plan §10: 1 GB per user, 100 MB per file, counted in binary units).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QuotaConfig {
    pub limit_bytes: u64,
    pub max_file_bytes: u64,
}

impl Default for QuotaConfig {
    fn default() -> Self {
        Self {
            limit_bytes: 1024 * MIB,
            max_file_bytes: 100 * MIB,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum QuotaError {
    #[error("file exceeds the per-file limit")]
    TooLarge,
    #[error("actor quota exceeded")]
    QuotaExceeded,
}

impl QuotaConfig {
    /// Whether `incoming` more bytes fit for an actor already using `used`.
    pub fn check(&self, used: u64, incoming: u64) -> Result<(), QuotaError> {
        if incoming > self.max_file_bytes {
            return Err(QuotaError::TooLarge);
        }
        match used.checked_add(incoming) {
            Some(total) if total <= self.limit_bytes => Ok(()),
            _ => Err(QuotaError::QuotaExceeded),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_follow_the_plan() {
        let q = QuotaConfig::default();
        assert_eq!(q.limit_bytes, 1_073_741_824);
        assert_eq!(q.max_file_bytes, 104_857_600);
    }

    #[test]
    fn per_file_limit_comes_first() {
        let q = QuotaConfig {
            limit_bytes: 10,
            max_file_bytes: 5,
        };
        assert_eq!(q.check(0, 6), Err(QuotaError::TooLarge));
        assert_eq!(q.check(10, 6), Err(QuotaError::TooLarge));
    }

    #[test]
    fn total_limit_is_inclusive() {
        let q = QuotaConfig {
            limit_bytes: 10,
            max_file_bytes: 5,
        };
        assert_eq!(q.check(5, 5), Ok(()));
        assert_eq!(q.check(6, 5), Err(QuotaError::QuotaExceeded));
        assert_eq!(q.check(u64::MAX, 1), Err(QuotaError::QuotaExceeded));
        assert_eq!(q.check(0, 0), Ok(()));
    }
}
