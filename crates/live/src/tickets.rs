//! One-time tickets that open a live connection. Browsers cannot put an `Authorization` header on
//! a WebSocket, so the client trades its token for a ticket and puts only that in the URL; the
//! ticket is short-lived, single-use, bound to one document and the client address, and held only
//! as its sha256. One holder (an account, or a link from one client network) keeps only its most
//! recent tickets; past the caps of a client network, of one owner's links, or of the store, a new
//! ticket is refused. Nobody's ticket is ever pushed out by someone else's.
use std::{
    collections::HashMap,
    net::IpAddr,
    sync::{Mutex, PoisonError},
    time::{Duration, Instant},
};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use ring::rand::{SecureRandom, SystemRandom};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{LiveConfig, quota_network};

const TICKET_BYTES: usize = 32;
/// base64url of 32 bytes, without padding.
const TICKET_CHARS: usize = 43;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TicketError {
    /// As many tickets are outstanding as the server, the client network or the owner's links
    /// may hold; try again shortly.
    Full,
    /// The system random source failed.
    Random,
}

struct Entry<T> {
    document: Uuid,
    address: Option<IpAddr>,
    holder_key: String,
    /// The document owner, for tickets issued through a public link.
    owner: Option<String>,
    holder: T,
    expires: Instant,
    issued: u64,
}

struct Store<T> {
    entries: HashMap<[u8; 32], Entry<T>>,
    next: u64,
}

/// Outstanding tickets; `T` is whoever the ticket was issued to.
pub struct Tickets<T> {
    ttl: Duration,
    max: usize,
    per_holder: usize,
    per_address: usize,
    per_owner: usize,
    store: Mutex<Store<T>>,
}

impl<T> Tickets<T> {
    pub fn new(config: &LiveConfig) -> Self {
        Self {
            ttl: config.ticket_ttl,
            max: config.max_tickets,
            per_holder: config.max_tickets_per_holder.max(1),
            per_address: config.max_tickets_per_address.max(1),
            per_owner: config.max_link_tickets_per_owner.max(1),
            store: Mutex::new(Store {
                entries: HashMap::new(),
                next: 0,
            }),
        }
    }

    pub fn ttl(&self) -> Duration {
        self.ttl
    }

    /// A ticket for `holder`; `holder_key` names it (an account, a link) for the quota, and
    /// `link_owner` is the document owner when it comes through a public link.
    pub fn issue(
        &self,
        document: Uuid,
        address: Option<IpAddr>,
        holder_key: &str,
        link_owner: Option<&str>,
        holder: T,
        now: Instant,
    ) -> Result<String, TicketError> {
        let mut bytes = [0u8; TICKET_BYTES];
        SystemRandom::new()
            .fill(&mut bytes)
            .map_err(|_| TicketError::Random)?;
        let ticket = URL_SAFE_NO_PAD.encode(bytes);
        let mut store = self.store.lock().unwrap_or_else(PoisonError::into_inner);
        // Full passes: the store holds at most `max` entries, and issuing is rate limited.
        store.entries.retain(|_, e| e.expires > now);
        // Only the holder's own tickets make room: a cap shared with others refuses instead, or one
        // person on a shared network or link could push out everyone else's tickets.
        evict_oldest(&mut store.entries, self.per_holder, |e| {
            e.holder_key == holder_key
        });
        let network = quota_network(address);
        let network_full =
            count(&store.entries, |e| quota_network(e.address) == network) >= self.per_address;
        let owner_full = link_owner.is_some_and(|owner| {
            count(&store.entries, |e| e.owner.as_deref() == Some(owner)) >= self.per_owner
        });
        if network_full || owner_full || store.entries.len() >= self.max {
            return Err(TicketError::Full);
        }
        let issued = store.next;
        store.next += 1;
        store.entries.insert(
            digest(&ticket),
            Entry {
                document,
                address,
                holder_key: holder_key.to_owned(),
                owner: link_owner.map(str::to_owned),
                holder,
                expires: now + self.ttl,
                issued,
            },
        );
        Ok(ticket)
    }

    /// The holder, when `ticket` is outstanding, unexpired, for `document` and used from the
    /// address it was issued to. Any attempt spends the ticket, matching or not.
    pub fn redeem(
        &self,
        ticket: &str,
        document: Uuid,
        address: Option<IpAddr>,
        now: Instant,
    ) -> Option<T> {
        let well_formed = ticket.len() == TICKET_CHARS
            && ticket
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
        if !well_formed {
            return None;
        }
        let entry = self
            .store
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entries
            .remove(&digest(ticket))?;
        (entry.expires > now && entry.document == document && entry.address == address)
            .then_some(entry.holder)
    }

    pub fn outstanding(&self) -> usize {
        self.store
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entries
            .len()
    }
}

fn count<T>(entries: &HashMap<[u8; 32], Entry<T>>, same: impl Fn(&Entry<T>) -> bool) -> usize {
    entries.values().filter(|e| same(e)).count()
}

/// Removes the oldest entries matching `same` until one more fits under `limit` (at least 1).
fn evict_oldest<T>(
    entries: &mut HashMap<[u8; 32], Entry<T>>,
    limit: usize,
    same: impl Fn(&Entry<T>) -> bool,
) {
    let mut matching: Vec<(u64, [u8; 32])> = entries
        .iter()
        .filter(|(_, e)| same(e))
        .map(|(key, e)| (e.issued, *key))
        .collect();
    if matching.len() < limit {
        return;
    }
    matching.sort_unstable();
    for (_, key) in &matching[..=matching.len() - limit] {
        entries.remove(key);
    }
}

fn digest(ticket: &str) -> [u8; 32] {
    Sha256::digest(ticket.as_bytes()).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    const TTL: Duration = Duration::from_secs(30);

    fn ip(s: &str) -> Option<IpAddr> {
        Some(s.parse().expect("ip"))
    }

    fn store<T>(max: usize, per_holder: usize, per_address: usize) -> Tickets<T> {
        Tickets::new(&LiveConfig {
            ticket_ttl: TTL,
            max_tickets: max,
            max_tickets_per_holder: per_holder,
            max_tickets_per_address: per_address,
            ..LiveConfig::default()
        })
    }

    #[test]
    fn addresses_of_one_ipv6_56_share_a_cap_that_refuses() {
        let tickets = store(100, 10, 2);
        let doc = Uuid::new_v4();
        let now = Instant::now();
        let issue = |key: &str, address: &str| tickets.issue(doc, ip(address), key, None, (), now);
        let first = issue("a", "2001:db8:0:100::1").expect("a");
        issue("b", "2001:db8:0:1ff::1").expect("b");
        assert_eq!(
            issue("c", "2001:db8:0:1a0::1"),
            Err(TicketError::Full),
            "a third holder from the same /56 is refused"
        );
        assert_eq!(
            tickets.redeem(&first, doc, ip("2001:db8:0:100::1"), now),
            Some(()),
            "and pushed nobody out"
        );
        let other = issue("d", "2001:db8:0:200::1").expect("another /56");
        assert_eq!(
            tickets.redeem(&other, doc, ip("2001:db8:0:200::1"), now),
            Some(())
        );
    }

    #[test]
    fn link_tickets_of_one_owner_are_capped_without_evicting_others() {
        let tickets = Tickets::new(&LiveConfig {
            max_link_tickets_per_owner: 2,
            ..LiveConfig::default()
        });
        let doc = Uuid::new_v4();
        let now = Instant::now();
        let issue = |key: &str, address: &str, owner: Option<&str>| {
            tickets.issue(doc, ip(address), key, owner, (), now)
        };
        let first = issue("link1", "10.0.0.1", Some("owner")).expect("1");
        issue("link2", "10.0.0.2", Some("owner")).expect("2");
        assert_eq!(
            issue("link1", "10.0.0.5", Some("owner")),
            Err(TicketError::Full)
        );
        let account = issue("actor", "10.0.0.3", None).expect("an account's is not a link's");
        let other_owner = issue("link3", "10.0.0.4", Some("other")).expect("other owner");
        assert_eq!(tickets.redeem(&first, doc, ip("10.0.0.1"), now), Some(()));
        assert_eq!(tickets.redeem(&account, doc, ip("10.0.0.3"), now), Some(()));
        assert_eq!(
            tickets.redeem(&other_owner, doc, ip("10.0.0.4"), now),
            Some(())
        );
        issue("link1", "10.0.0.5", Some("owner")).expect("a redeemed one made room");
    }

    #[test]
    fn a_ticket_opens_once_for_its_document_and_address() {
        let tickets = store(10, 10, 10);
        let (doc, other) = (Uuid::new_v4(), Uuid::new_v4());
        let now = Instant::now();
        let t = tickets
            .issue(doc, ip("10.0.0.1"), "k", None, "a", now)
            .expect("ticket");
        assert_eq!(t.len(), TICKET_CHARS);
        assert_eq!(tickets.redeem(&t, doc, ip("10.0.0.1"), now), Some("a"));
        assert_eq!(tickets.redeem(&t, doc, ip("10.0.0.1"), now), None, "reused");

        let t = tickets
            .issue(doc, ip("10.0.0.1"), "k", None, "b", now)
            .expect("ticket");
        assert_eq!(tickets.redeem(&t, other, ip("10.0.0.1"), now), None);
        assert_eq!(tickets.redeem(&t, doc, ip("10.0.0.1"), now), None, "spent");

        let t = tickets
            .issue(doc, ip("10.0.0.1"), "k", None, "c", now)
            .expect("ticket");
        assert_eq!(tickets.redeem(&t, doc, ip("10.0.0.2"), now), None);

        let t = tickets
            .issue(doc, None, "k", None, "d", now)
            .expect("ticket");
        assert_eq!(tickets.redeem(&t, doc, None, now + TTL), None, "expired");
        assert_eq!(tickets.outstanding(), 0);
    }

    #[test]
    fn malformed_tickets_and_a_full_store_are_refused() {
        let tickets = store(2, 10, 10);
        let doc = Uuid::new_v4();
        let now = Instant::now();
        for bad in ["", "short", &"a".repeat(44), &"\u{e9}".repeat(22)] {
            assert_eq!(tickets.redeem(bad, doc, None, now), None);
        }
        tickets.issue(doc, None, "k", None, 1, now).expect("first");
        tickets.issue(doc, None, "k", None, 2, now).expect("second");
        assert_eq!(
            tickets.issue(doc, None, "k", None, 3, now),
            Err(TicketError::Full)
        );
        tickets
            .issue(doc, None, "k", None, 4, now + TTL)
            .expect("expired ones make room");
        assert_eq!(tickets.outstanding(), 1);
    }

    #[test]
    fn a_holder_keeps_its_most_recent_tickets_and_a_full_network_refuses() {
        let tickets = store(100, 2, 3);
        let doc = Uuid::new_v4();
        let now = Instant::now();
        let issue =
            |key: &str, address: &str, n: u32| tickets.issue(doc, ip(address), key, None, n, now);
        let first = issue("a", "10.0.0.1", 1).expect("1");
        let second = issue("a", "10.0.0.2", 2).expect("2");
        let third = issue("a", "10.0.0.3", 3).expect("3");
        assert_eq!(tickets.redeem(&first, doc, ip("10.0.0.1"), now), None);

        let from_b = issue("b", "10.0.0.9", 4).expect("4");
        let from_c = issue("c", "10.0.0.9", 5).expect("5");
        let from_d = issue("d", "10.0.0.9", 6).expect("6");
        assert_eq!(issue("e", "10.0.0.9", 7), Err(TicketError::Full));
        for (t, n) in [(&from_b, 4), (&from_c, 5), (&from_d, 6)] {
            assert_eq!(tickets.redeem(t, doc, ip("10.0.0.9"), now), Some(n));
        }
        assert_eq!(tickets.redeem(&second, doc, ip("10.0.0.2"), now), Some(2));
        assert_eq!(tickets.redeem(&third, doc, ip("10.0.0.3"), now), Some(3));
        assert_eq!(tickets.outstanding(), 0);
    }
}
