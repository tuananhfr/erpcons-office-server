use std::{
    collections::HashMap,
    fmt,
    sync::{Mutex, MutexGuard, PoisonError},
    time::{Duration, Instant},
};

use reqwest::{StatusCode, header::ACCEPT};
use serde::Deserialize;
use tracing::warn;

use crate::{EmailClaim, http, single_flight::SingleFlight};

const MAX_USERINFO_BYTES: usize = 16 * 1024;
const MAX_NAME_CHARS: usize = 255;
/// RFC 5321's limit on a path; anything longer is not a usable address.
const MAX_EMAIL_CHARS: usize = 254;

/// What PI's userinfo says about an actor: the display name, and whether its email is verified
/// (an unverified one could claim someone else's invitations).
#[derive(Clone, Default, PartialEq, Eq)]
pub(crate) struct Profile {
    pub(crate) name: String,
    pub(crate) email: EmailClaim,
}

// Personal data: `{:?}` must not print it.
impl fmt::Debug for Profile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Profile(<redacted>)")
    }
}

/// PI profiles by actor. The access token carries none, so PI's userinfo is asked once per actor
/// and `ttl`; when PI cannot answer, the last known profile (or an empty one) stands in.
pub(crate) struct Profiles {
    http: reqwest::Client,
    url: String,
    timeout: Duration,
    ttl: Duration,
    retry: Duration,
    max_entries: usize,
    entries: Mutex<HashMap<String, Entry>>,
    inflight: SingleFlight<String, Profile>,
}

struct Entry {
    profile: Profile,
    next_fetch: Instant,
}

pub(crate) struct ProfilesConfig {
    pub(crate) url: String,
    pub(crate) timeout: Duration,
    pub(crate) ttl: Duration,
    pub(crate) retry: Duration,
    pub(crate) max_entries: usize,
}

#[derive(Deserialize)]
struct UserInfo {
    sub: String,
    // Present only when the token has the `profile` scope.
    name: Option<String>,
    // Present only when the token has the `email` scope (OIDC Core §5.4).
    email: Option<String>,
    email_verified: Option<bool>,
}

impl UserInfo {
    fn into_profile(self) -> Profile {
        let email = match (self.email, self.email_verified) {
            (None, None) => EmailClaim::Unknown,
            (Some(email), Some(true)) => Some(email.trim().to_owned())
                .filter(|e| !e.is_empty() && e.chars().count() <= MAX_EMAIL_CHARS)
                .map_or(EmailClaim::Unverified, EmailClaim::Verified),
            _ => EmailClaim::Unverified,
        };
        Profile {
            name: self
                .name
                .unwrap_or_default()
                .chars()
                .take(MAX_NAME_CHARS)
                .collect(),
            email,
        }
    }
}

impl Profiles {
    pub(crate) fn new(http: reqwest::Client, config: ProfilesConfig) -> Self {
        Self {
            http,
            url: config.url,
            timeout: config.timeout,
            ttl: config.ttl,
            retry: config.retry,
            max_entries: config.max_entries,
            entries: Mutex::new(HashMap::new()),
            inflight: SingleFlight::new(),
        }
    }

    /// The profile of `actor_id`, whose verified `token` may be shown to PI to ask for it. Never fails.
    pub(crate) async fn get(&self, actor_id: &str, token: &str) -> Profile {
        if let Some(entry) = self.lock().get(actor_id)
            && entry.next_fetch > Instant::now()
        {
            return entry.profile.clone();
        }
        self.inflight
            .run(actor_id.to_owned(), || async {
                let fetched = self.fetch(actor_id, token).await;
                let now = Instant::now();
                let mut entries = self.lock();
                let (profile, next_fetch) = match fetched {
                    Some(profile) => (profile, now + self.ttl),
                    None => (
                        entries
                            .get(actor_id)
                            .map(|e| e.profile.clone())
                            .unwrap_or_default(),
                        now + self.retry,
                    ),
                };
                if entries.len() >= self.max_entries && !entries.contains_key(actor_id) {
                    entries.retain(|_, e| e.next_fetch > now);
                }
                if entries.len() < self.max_entries || entries.contains_key(actor_id) {
                    entries.insert(
                        actor_id.to_owned(),
                        Entry {
                            profile: profile.clone(),
                            next_fetch,
                        },
                    );
                }
                profile
            })
            .await
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<String, Entry>> {
        self.entries.lock().unwrap_or_else(PoisonError::into_inner)
    }

    async fn fetch(&self, actor_id: &str, token: &str) -> Option<Profile> {
        let response = self
            .http
            .get(&self.url)
            .bearer_auth(token)
            .header(ACCEPT, "application/json")
            .timeout(self.timeout)
            .send()
            .await
            .map_err(|e| {
                warn!(
                    timeout = e.is_timeout(),
                    connect = e.is_connect(),
                    "PI userinfo unreachable"
                );
            })
            .ok()?;
        if response.status() != StatusCode::OK {
            warn!(status = response.status().as_u16(), "PI userinfo failed");
            return None;
        }
        let info = http::read_capped(response, MAX_USERINFO_BYTES)
            .await
            .and_then(|body| serde_json::from_slice::<UserInfo>(&body).ok());
        let Some(info) = info else {
            warn!("PI userinfo returned an unreadable body");
            return None;
        };
        if info.sub != actor_id {
            warn!("PI userinfo answered for another subject");
            return None;
        }
        Some(info.into_profile())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(email: Option<&str>, verified: Option<bool>) -> UserInfo {
        UserInfo {
            sub: "usr_1".into(),
            name: Some("An".into()),
            email: email.map(str::to_owned),
            email_verified: verified,
        }
    }

    #[test]
    fn only_a_verified_email_is_kept() {
        let verified = info(Some(" an@example.vn "), Some(true)).into_profile();
        assert_eq!(verified.email.verified(), Some("an@example.vn"));
        for (email, flag) in [
            (Some("an@example.vn"), Some(false)),
            (Some("an@example.vn"), None),
            (None, Some(true)),
            (None, Some(false)),
            (Some("  "), Some(true)),
        ] {
            assert_eq!(
                info(email, flag).into_profile().email,
                EmailClaim::Unverified
            );
        }
        // No email claim at all (no `email` scope) says nothing either way.
        assert_eq!(info(None, None).into_profile().email, EmailClaim::Unknown);
    }

    #[test]
    fn debug_hides_the_profile() {
        let p = info(Some("an@example.vn"), Some(true)).into_profile();
        assert!(!format!("{p:?}").contains("example"));
    }
}
