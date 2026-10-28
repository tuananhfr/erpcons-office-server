use std::{
    collections::HashMap,
    sync::{Mutex, MutexGuard, PoisonError},
    time::{Duration, Instant},
};

use reqwest::{StatusCode, header::ACCEPT};
use serde::Deserialize;
use tracing::warn;

use crate::{http, single_flight::SingleFlight};

const MAX_USERINFO_BYTES: usize = 16 * 1024;
const MAX_NAME_CHARS: usize = 255;

/// PI display names by actor. The access token carries none, so PI's userinfo is asked once per
/// actor and `ttl`; when PI cannot answer, the last known name (or an empty one) stands in.
pub(crate) struct DisplayNames {
    http: reqwest::Client,
    url: String,
    timeout: Duration,
    ttl: Duration,
    retry: Duration,
    max_entries: usize,
    entries: Mutex<HashMap<String, Entry>>,
    inflight: SingleFlight<String, String>,
}

struct Entry {
    name: String,
    next_fetch: Instant,
}

pub(crate) struct DisplayNamesConfig {
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
}

impl DisplayNames {
    pub(crate) fn new(http: reqwest::Client, config: DisplayNamesConfig) -> Self {
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

    /// The name of `actor_id`, whose verified `token` may be shown to PI to ask for it. Never fails.
    pub(crate) async fn get(&self, actor_id: &str, token: &str) -> String {
        if let Some(entry) = self.lock().get(actor_id)
            && entry.next_fetch > Instant::now()
        {
            return entry.name.clone();
        }
        self.inflight
            .run(actor_id.to_owned(), || async {
                let fetched = self.fetch(actor_id, token).await;
                let now = Instant::now();
                let mut entries = self.lock();
                let (name, next_fetch) = match fetched {
                    Some(name) => (name, now + self.ttl),
                    None => (
                        entries
                            .get(actor_id)
                            .map(|e| e.name.clone())
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
                            name: name.clone(),
                            next_fetch,
                        },
                    );
                }
                name
            })
            .await
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<String, Entry>> {
        self.entries.lock().unwrap_or_else(PoisonError::into_inner)
    }

    async fn fetch(&self, actor_id: &str, token: &str) -> Option<String> {
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
        Some(
            info.name
                .unwrap_or_default()
                .chars()
                .take(MAX_NAME_CHARS)
                .collect(),
        )
    }
}
