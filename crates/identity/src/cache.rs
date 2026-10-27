use std::{
    collections::HashMap,
    sync::{Mutex, PoisonError},
    time::{Duration, Instant},
};

use sha2::{Digest, Sha256};

use crate::{Actor, VerifyError};

/// The token's sha256, so the cache never holds a usable token.
pub(crate) type TokenHash = [u8; 32];

pub(crate) fn hash_token(token: &str) -> TokenHash {
    Sha256::digest(token.as_bytes()).into()
}

struct Entry {
    verdict: Result<Actor, VerifyError>,
    until: Instant,
}

/// Recent verdicts by token hash, bounded in size.
pub(crate) struct VerdictCache {
    entries: Mutex<HashMap<TokenHash, Entry>>,
    max_entries: usize,
}

impl VerdictCache {
    pub(crate) fn new(max_entries: usize) -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            max_entries,
        }
    }

    pub(crate) fn get(&self, key: &TokenHash, now: Instant) -> Option<Result<Actor, VerifyError>> {
        let entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        entries
            .get(key)
            .filter(|e| e.until > now)
            .map(|e| e.verdict.clone())
    }

    pub(crate) fn insert(
        &self,
        key: TokenHash,
        verdict: Result<Actor, VerifyError>,
        ttl: Duration,
        now: Instant,
    ) {
        if ttl.is_zero() {
            return;
        }
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        if entries.len() >= self.max_entries && !entries.contains_key(&key) {
            entries.retain(|_, e| e.until > now);
            // Still full of live entries: skip caching rather than grow without bound.
            if entries.len() >= self.max_entries {
                return;
            }
        }
        entries.insert(
            key,
            Entry {
                verdict,
                until: now + ttl,
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use time::OffsetDateTime;

    use super::*;

    fn actor() -> Actor {
        Actor {
            actor_id: "usr_1".into(),
            display_name: "An".into(),
            expires_at: OffsetDateTime::UNIX_EPOCH,
        }
    }

    #[test]
    fn entries_expire() {
        let cache = VerdictCache::new(10);
        let now = Instant::now();
        let key = hash_token("t");
        cache.insert(key, Ok(actor()), Duration::from_secs(60), now);
        assert_eq!(cache.get(&key, now), Some(Ok(actor())));
        assert_eq!(cache.get(&key, now + Duration::from_secs(61)), None);
    }

    #[test]
    fn zero_ttl_is_not_cached() {
        let cache = VerdictCache::new(10);
        let now = Instant::now();
        let key = hash_token("t");
        cache.insert(key, Err(VerifyError::Unavailable), Duration::ZERO, now);
        assert_eq!(cache.get(&key, now), None);
    }

    #[test]
    fn size_is_bounded_and_expired_entries_make_room() {
        let cache = VerdictCache::new(2);
        let now = Instant::now();
        let ttl = Duration::from_secs(10);
        cache.insert(hash_token("a"), Ok(actor()), ttl, now);
        cache.insert(hash_token("b"), Ok(actor()), ttl, now);
        cache.insert(hash_token("c"), Ok(actor()), ttl, now);
        assert_eq!(cache.get(&hash_token("c"), now), None);
        let later = now + Duration::from_secs(11);
        cache.insert(hash_token("c"), Ok(actor()), ttl, later);
        assert_eq!(cache.get(&hash_token("c"), later), Some(Ok(actor())));
    }

    #[test]
    fn keys_are_token_hashes() {
        assert_ne!(hash_token("a"), hash_token("b"));
        assert_eq!(hash_token("a").len(), 32);
    }
}
