//! One-time tickets that open a live connection. Browsers cannot put an `Authorization` header on
//! a WebSocket, so the client trades its token for a ticket and puts only that in the URL; the
//! ticket is short-lived, single-use, bound to one document and the client address, and held only
//! as its sha256. Caps bound what one holder (an account, or a link from one client network), one
//! client network, one owner's public links and the whole store hold. A new ticket past a cap
//! pushes out the oldest ticket of whoever holds the most there (holders within a network, client
//! networks across them), but someone else's only while they would still hold more than the
//! requester; else the requester's own, and with none it is refused. Spreading tickets over many
//! keys or networks so never pushes out a light user's ticket.
use std::{
    cmp::Reverse,
    collections::{HashMap, HashSet},
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
    /// The client address is unknown; a shared unknown bucket would let anyone crowd it.
    NoAddress,
    /// A cap is full and nobody in it holds more than the requester would.
    Full,
    /// The system random source failed.
    Random,
}

struct Entry<T> {
    document: Uuid,
    address: Option<IpAddr>,
    /// The client network the caps count (`quota_network`).
    network: Option<IpAddr>,
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

    /// A ticket for `holder`; `holder_key` names it (an account, or a link from one client
    /// network) for the caps, and `link_owner` is the document owner when it comes through a public
    /// link.
    pub fn issue(
        &self,
        document: Uuid,
        address: Option<IpAddr>,
        holder_key: &str,
        link_owner: Option<&str>,
        holder_value: T,
        now: Instant,
    ) -> Result<String, TicketError> {
        if address.is_none() {
            return Err(TicketError::NoAddress);
        }
        let mut bytes = [0u8; TICKET_BYTES];
        SystemRandom::new()
            .fill(&mut bytes)
            .map_err(|_| TicketError::Random)?;
        let ticket = URL_SAFE_NO_PAD.encode(bytes);
        let network = quota_network(address);
        let mut store = self.store.lock().unwrap_or_else(PoisonError::into_inner);
        // Full passes: the store holds at most `max` entries, and issuing is rate limited.
        store.entries.retain(|_, e| e.expires > now);
        let entries = &store.entries;
        let holder = Group::Holder(holder_key);
        let from = Group::Network(network);
        // Victims are only chosen here and removed once every cap made room: a refusal evicts
        // nothing.
        let mut evict = HashSet::new();
        let mut make_room = |requester, limit, in_cap: &dyn Fn(&Entry<T>) -> bool| {
            choose_victims(entries, &mut evict, requester, limit, in_cap)
        };
        make_room(holder, self.per_holder, &|e| e.holder_key == holder_key)?;
        // One holder takes at most a quarter of its network's share, one network an eighth of an
        // owner's links: filling a shared cap takes several of them.
        make_room(holder, (self.per_address / 4).max(1), &|e| {
            e.holder_key == holder_key && e.network == network
        })?;
        if let Some(owner) = link_owner {
            make_room(holder, (self.per_owner / 8).max(1), &|e| {
                e.owner.as_deref() == Some(owner) && e.network == network
            })?;
        }
        make_room(holder, self.per_address, &|e| e.network == network)?;
        // Across networks fairness counts networks: a link holder's key is cheap to multiply.
        if let Some(owner) = link_owner {
            make_room(from, self.per_owner, &|e| e.owner.as_deref() == Some(owner))?;
        }
        make_room(from, self.max, &|_| true)?;
        for key in evict {
            store.entries.remove(&key);
        }
        let issued = store.next;
        store.next += 1;
        store.entries.insert(
            digest(&ticket),
            Entry {
                document,
                address,
                network,
                holder_key: holder_key.to_owned(),
                owner: link_owner.map(str::to_owned),
                holder: holder_value,
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

/// Whom a cap weighs against each other: holders, or client networks.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Group<'a> {
    Holder(&'a str),
    Network(Option<IpAddr>),
}

impl<'a> Group<'a> {
    fn of<T>(self, e: &'a Entry<T>) -> Self {
        match self {
            Self::Holder(_) => Self::Holder(&e.holder_key),
            Self::Network(_) => Self::Network(e.network),
        }
    }
}

/// Adds to `evict` until one more ticket fits under `limit` among the entries `in_cap` not yet in
/// it: the oldest ticket of the heaviest other group while it holds at least two more than
/// `requester`, else the requester's own oldest; `Full` when the requester has none there.
fn choose_victims<'a, T>(
    entries: &'a HashMap<[u8; 32], Entry<T>>,
    evict: &mut HashSet<[u8; 32]>,
    requester: Group<'a>,
    limit: usize,
    in_cap: &dyn Fn(&Entry<T>) -> bool,
) -> Result<(), TicketError> {
    loop {
        // Per group: tickets in the cap, and its oldest (issue number, key).
        let mut groups: HashMap<Group<'_>, (usize, (u64, [u8; 32]))> = HashMap::new();
        let mut total = 0;
        let left = entries
            .iter()
            .filter(|(key, e)| !evict.contains(*key) && in_cap(e));
        for (key, e) in left {
            total += 1;
            let slot = groups
                .entry(requester.of(e))
                .or_insert((0, (e.issued, *key)));
            slot.0 += 1;
            slot.1 = slot.1.min((e.issued, *key));
        }
        if total < limit.max(1) {
            return Ok(());
        }
        let mine = groups.get(&requester).copied();
        let held = mine.map_or(0, |g| g.0);
        let heaviest = groups
            .iter()
            .filter(|(group, _)| **group != requester)
            .map(|(_, g)| *g)
            .max_by_key(|g| (g.0, Reverse(g.1)));
        // Equal after the request is not heavier: taking from a tie lets many light keys evict.
        let victim = match (heaviest, mine) {
            (Some(other), _) if other.0 > held + 1 => other,
            (_, Some(own)) => own,
            _ => return Err(TicketError::Full),
        };
        evict.insert((victim.1).1);
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

    /// Whether `ticket` is still outstanding, without spending it.
    fn held<T>(tickets: &Tickets<T>, ticket: &str) -> bool {
        tickets
            .store
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entries
            .contains_key(&digest(ticket))
    }

    #[test]
    fn a_ticket_opens_once_for_its_document_and_address() {
        let tickets = store(10, 10, 10);
        let (doc, other) = (Uuid::new_v4(), Uuid::new_v4());
        let now = Instant::now();
        let a = ip("10.0.0.1");
        let t = tickets.issue(doc, a, "k", None, "a", now).expect("ticket");
        assert_eq!(t.len(), TICKET_CHARS);
        assert_eq!(tickets.redeem(&t, doc, a, now), Some("a"));
        assert_eq!(tickets.redeem(&t, doc, a, now), None, "reused");

        let t = tickets.issue(doc, a, "k", None, "b", now).expect("ticket");
        assert_eq!(tickets.redeem(&t, other, a, now), None);
        assert_eq!(tickets.redeem(&t, doc, a, now), None, "spent");

        let t = tickets.issue(doc, a, "k", None, "c", now).expect("ticket");
        assert_eq!(tickets.redeem(&t, doc, ip("10.0.0.2"), now), None);

        let t = tickets.issue(doc, a, "k", None, "d", now).expect("ticket");
        assert_eq!(tickets.redeem(&t, doc, a, now + TTL), None, "expired");
        assert_eq!(tickets.outstanding(), 0);
    }

    #[test]
    fn malformed_tickets_and_unknown_addresses_are_refused() {
        let tickets = store(10, 10, 10);
        let doc = Uuid::new_v4();
        let now = Instant::now();
        for bad in ["", "short", &"a".repeat(44), &"\u{e9}".repeat(22)] {
            assert_eq!(tickets.redeem(bad, doc, ip("10.0.0.1"), now), None);
        }
        assert_eq!(
            tickets.issue(doc, None, "k", None, 1, now),
            Err(TicketError::NoAddress)
        );
        assert_eq!(tickets.outstanding(), 0);
    }

    #[test]
    fn a_holder_keeps_its_most_recent_tickets() {
        let tickets = store(100, 2, 100);
        let doc = Uuid::new_v4();
        let now = Instant::now();
        let issue = |n: u32, address: &str| {
            tickets
                .issue(doc, ip(address), "a", None, n, now)
                .expect("ticket")
        };
        let first = issue(1, "10.0.0.1");
        let second = issue(2, "10.0.0.2");
        let third = issue(3, "10.0.0.3");
        assert_eq!(tickets.redeem(&first, doc, ip("10.0.0.1"), now), None);
        assert_eq!(tickets.redeem(&second, doc, ip("10.0.0.2"), now), Some(2));
        assert_eq!(tickets.redeem(&third, doc, ip("10.0.0.3"), now), Some(3));
    }

    #[test]
    fn whoever_fills_a_network_pushes_out_its_own_tickets_only() {
        // A /56 holds 8 tickets; one holder at most a quarter of that, 2.
        let tickets = store(100, 20, 8);
        let doc = Uuid::new_v4();
        let now = Instant::now();
        let issue = |key: &str, address: &str| {
            tickets
                .issue(doc, ip(address), key, None, (), now)
                .expect("ticket")
        };
        let victim = issue("victim", "2001:db8:0:100::1");
        let mut attacker = Vec::new();
        for n in 0..20 {
            attacker.push(issue(&format!("attacker{}", n % 4), "2001:db8:0:1ff::1"));
        }
        assert!(held(&tickets, &victim), "the light holder keeps its ticket");
        assert_eq!(
            tickets.redeem(&victim, doc, ip("2001:db8:0:100::1"), now),
            Some(())
        );
        assert!(
            !held(&tickets, &attacker[0]),
            "the attackers lost their oldest"
        );
        assert!(held(&tickets, &attacker[19]));
        let elsewhere = issue("other", "2001:db8:0:200::1");
        assert!(held(&tickets, &elsewhere));
    }

    #[test]
    fn one_holder_takes_at_most_a_quarter_of_its_network() {
        let tickets = store(100, 20, 8);
        let doc = Uuid::new_v4();
        let now = Instant::now();
        let issue = |address: &str| {
            tickets
                .issue(doc, ip(address), "a", None, (), now)
                .expect("ticket")
        };
        let first = issue("10.0.0.1");
        let second = issue("10.0.0.1");
        let third = issue("10.0.0.1");
        assert!(!held(&tickets, &first));
        assert!(held(&tickets, &second) && held(&tickets, &third));
        let elsewhere = issue("10.0.0.2");
        assert!(
            held(&tickets, &second),
            "another network has its own quarter"
        );
        assert!(held(&tickets, &elsewhere));
    }

    #[test]
    fn the_requester_evicts_its_own_ticket_on_a_tie() {
        // Eight holders with one ticket each fill the network; the quarter (2) is not reached.
        let tickets = store(100, 20, 8);
        let doc = Uuid::new_v4();
        let now = Instant::now();
        let issue = |key: &str| {
            tickets
                .issue(doc, ip("10.0.0.1"), key, None, (), now)
                .expect("ticket")
        };
        let others: Vec<String> = ["b", "c", "d", "e", "f", "g", "h"]
            .into_iter()
            .map(issue)
            .collect();
        let a1 = issue("a");
        let a2 = issue("a");
        assert!(others.iter().all(|t| held(&tickets, t)));
        assert!(
            !held(&tickets, &a1),
            "a, tied with the others, pushed out its own"
        );
        assert!(held(&tickets, &a2));
    }

    #[test]
    fn many_keys_with_one_ticket_each_cannot_push_out_a_light_holder() {
        let tickets = store(100, 20, 8);
        let doc = Uuid::new_v4();
        let now = Instant::now();
        let victim = tickets
            .issue(doc, ip("10.0.0.1"), "victim", None, (), now)
            .expect("ticket");
        let refused = (0..20)
            .map(|n| tickets.issue(doc, ip("10.0.0.1"), &format!("k{n}"), None, (), now))
            .filter(|t| *t == Err(TicketError::Full))
            .count();
        assert_eq!(
            refused, 13,
            "seven fit beside the victim, then nobody is heavier"
        );
        assert_eq!(tickets.redeem(&victim, doc, ip("10.0.0.1"), now), Some(()));
    }

    #[test]
    fn a_heavy_holder_is_pushed_out_first() {
        // The quarter of a network of 8 is 2: "heavy" holds 2, a newcomer none.
        let tickets = store(100, 20, 8);
        let doc = Uuid::new_v4();
        let now = Instant::now();
        let issue = |key: &str| {
            tickets
                .issue(doc, ip("10.0.0.1"), key, None, (), now)
                .expect("ticket")
        };
        let heavy = [issue("heavy"), issue("heavy")];
        let light: Vec<String> = ["b", "c", "d", "e", "f", "g"]
            .into_iter()
            .map(issue)
            .collect();
        let newcomer = issue("new");
        assert!(!held(&tickets, &heavy[0]) && held(&tickets, &heavy[1]));
        assert!(light.iter().all(|t| held(&tickets, t)));
        assert!(held(&tickets, &newcomer));
    }

    #[test]
    fn the_whole_store_weighs_networks_not_keys() {
        let tickets = store(4, 20, 100);
        let doc = Uuid::new_v4();
        let now = Instant::now();
        let issue = |key: &str, n: u8| {
            tickets.issue(doc, Some(IpAddr::from([10, 0, 0, n])), key, None, (), now)
        };
        let light = issue("light", 1).expect("ticket");
        let heavy: Vec<String> = ["h1", "h2", "h3"]
            .into_iter()
            .map(|key| issue(key, 2).expect("ticket"))
            .collect();
        let newcomer = issue("new", 9).expect("ticket");
        assert!(held(&tickets, &light));
        assert!(!held(&tickets, &heavy[0]), "one network under three keys");
        assert!(held(&tickets, &newcomer));
        assert_eq!(tickets.outstanding(), 4);

        let spread = store::<()>(4, 20, 100);
        for n in 1..5 {
            spread
                .issue(doc, Some(IpAddr::from([10, 0, 0, n])), "k", None, (), now)
                .expect("ticket");
        }
        assert_eq!(
            spread.issue(doc, ip("10.0.0.9"), "new", None, (), now),
            Err(TicketError::Full),
            "nobody holds more than a newcomer would"
        );
    }

    /// Every outstanding ticket with its holder key, issue number and expiry, in a fixed order.
    fn snapshot<T>(tickets: &Tickets<T>) -> Vec<([u8; 32], String, u64, Instant)> {
        let store = tickets.store.lock().unwrap_or_else(PoisonError::into_inner);
        let mut all: Vec<_> = store
            .entries
            .iter()
            .map(|(k, e)| (*k, e.holder_key.clone(), e.issued, e.expires))
            .collect();
        all.sort();
        all
    }

    #[test]
    fn a_refused_request_evicts_nothing() {
        // A network of 8 holds "x" at its quarter (2) and six others; the owner's 2 links are taken
        // from two other networks.
        let tickets = Tickets::new(&LiveConfig {
            ticket_ttl: TTL,
            max_tickets_per_address: 8,
            max_link_tickets_per_owner: 2,
            ..LiveConfig::default()
        });
        let doc = Uuid::new_v4();
        let now = Instant::now();
        let issue = |key: &str, address: &str, owner: Option<&str>| {
            tickets
                .issue(doc, ip(address), key, owner, (), now)
                .expect("ticket")
        };
        let x = [issue("x", "10.0.0.9", None), issue("x", "10.0.0.9", None)];
        for key in ["b", "c", "d", "e", "f", "g"] {
            issue(key, "10.0.0.9", None);
        }
        issue("link:1", "10.0.0.1", Some("owner"));
        issue("link:2", "10.0.0.2", Some("owner"));
        let before = snapshot(&tickets);
        assert_eq!(
            tickets.issue(doc, ip("10.0.0.9"), "link:9", Some("owner"), (), now),
            Err(TicketError::Full)
        );
        assert_eq!(snapshot(&tickets), before, "x keeps both tickets");
        assert!(held(&tickets, &x[0]));
    }

    #[test]
    fn one_owners_links_are_capped_by_the_heaviest_and_per_network() {
        let tickets = Tickets::new(&LiveConfig {
            max_link_tickets_per_owner: 8,
            ..LiveConfig::default()
        });
        let doc = Uuid::new_v4();
        let now = Instant::now();
        let issue = |key: &str, address: &str, owner: Option<&str>| {
            tickets
                .issue(doc, ip(address), key, owner, (), now)
                .expect("ticket")
        };
        let victim = issue("link1:a", "10.0.0.1", Some("owner"));
        // One network takes an eighth of the owner's links: 1 ticket.
        let first = issue("link2:b", "10.0.0.2", Some("owner"));
        let second = issue("link2:b", "10.0.0.2", Some("owner"));
        assert!(!held(&tickets, &first) && held(&tickets, &second));
        assert_eq!(
            tickets.issue(doc, ip("10.0.0.2"), "link3:b", Some("owner"), (), now),
            Err(TicketError::Full),
            "another link from that network does not take its place"
        );
        let refused = (3..20)
            .map(|n| {
                tickets.issue(
                    doc,
                    ip(&format!("10.0.1.{n}")),
                    &format!("link9:{n}"),
                    Some("owner"),
                    (),
                    now,
                )
            })
            .filter(|t| *t == Err(TicketError::Full))
            .count();
        assert_eq!(
            refused, 11,
            "six networks fit, then none holds more than a newcomer"
        );
        let account = issue("actor:z", "10.0.0.5", None);
        assert!(held(&tickets, &victim));
        assert!(held(&tickets, &second));
        assert!(
            held(&tickets, &account),
            "an account's ticket is not a link's"
        );
        let other_owner = issue("link5:c", "10.0.0.6", Some("other"));
        assert!(held(&tickets, &other_owner));
    }
}
