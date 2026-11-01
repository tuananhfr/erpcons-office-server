//! One-time tickets that open a live connection. Browsers cannot put an `Authorization` header on
//! a WebSocket, so the client trades its token for a ticket and puts only that in the URL; the
//! ticket is short-lived, single-use, bound to one document and the client address, and held only
//! as its sha256.
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

const TICKET_BYTES: usize = 32;
/// base64url of 32 bytes, without padding.
const TICKET_CHARS: usize = 43;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TicketError {
    /// As many tickets are outstanding as the server holds.
    Full,
    /// The system random source failed.
    Random,
}

struct Entry<T> {
    document: Uuid,
    address: Option<IpAddr>,
    holder: T,
    expires: Instant,
}

/// Outstanding tickets; `T` is whoever the ticket was issued to.
pub struct Tickets<T> {
    ttl: Duration,
    max: usize,
    entries: Mutex<HashMap<[u8; 32], Entry<T>>>,
}

impl<T> Tickets<T> {
    pub fn new(ttl: Duration, max: usize) -> Self {
        Self {
            ttl,
            max,
            entries: Mutex::new(HashMap::new()),
        }
    }

    pub fn ttl(&self) -> Duration {
        self.ttl
    }

    pub fn issue(
        &self,
        document: Uuid,
        address: Option<IpAddr>,
        holder: T,
        now: Instant,
    ) -> Result<String, TicketError> {
        let mut bytes = [0u8; TICKET_BYTES];
        SystemRandom::new()
            .fill(&mut bytes)
            .map_err(|_| TicketError::Random)?;
        let ticket = URL_SAFE_NO_PAD.encode(bytes);
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        if entries.len() >= self.max {
            entries.retain(|_, e| e.expires > now);
            if entries.len() >= self.max {
                return Err(TicketError::Full);
            }
        }
        entries.insert(
            digest(&ticket),
            Entry {
                document,
                address,
                holder,
                expires: now + self.ttl,
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
            .entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&digest(ticket))?;
        (entry.expires > now && entry.document == document && entry.address == address)
            .then_some(entry.holder)
    }

    pub fn outstanding(&self) -> usize {
        self.entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
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

    #[test]
    fn a_ticket_opens_once_for_its_document_and_address() {
        let tickets = Tickets::new(TTL, 10);
        let (doc, other) = (Uuid::new_v4(), Uuid::new_v4());
        let now = Instant::now();
        let t = tickets
            .issue(doc, ip("10.0.0.1"), "a", now)
            .expect("ticket");
        assert_eq!(t.len(), TICKET_CHARS);
        assert_eq!(tickets.redeem(&t, doc, ip("10.0.0.1"), now), Some("a"));
        assert_eq!(tickets.redeem(&t, doc, ip("10.0.0.1"), now), None, "reused");

        let t = tickets
            .issue(doc, ip("10.0.0.1"), "b", now)
            .expect("ticket");
        assert_eq!(tickets.redeem(&t, other, ip("10.0.0.1"), now), None);
        assert_eq!(tickets.redeem(&t, doc, ip("10.0.0.1"), now), None, "spent");

        let t = tickets
            .issue(doc, ip("10.0.0.1"), "c", now)
            .expect("ticket");
        assert_eq!(tickets.redeem(&t, doc, ip("10.0.0.2"), now), None);

        let t = tickets.issue(doc, None, "d", now).expect("ticket");
        assert_eq!(tickets.redeem(&t, doc, None, now + TTL), None, "expired");
        assert_eq!(tickets.outstanding(), 0);
    }

    #[test]
    fn malformed_tickets_and_a_full_store_are_refused() {
        let tickets = Tickets::new(TTL, 2);
        let doc = Uuid::new_v4();
        let now = Instant::now();
        for bad in ["", "short", &"a".repeat(44), &"\u{e9}".repeat(22)] {
            assert_eq!(tickets.redeem(bad, doc, None, now), None);
        }
        tickets.issue(doc, None, 1, now).expect("first");
        tickets.issue(doc, None, 2, now).expect("second");
        assert_eq!(tickets.issue(doc, None, 3, now), Err(TicketError::Full));
        tickets
            .issue(doc, None, 4, now + TTL)
            .expect("expired ones make room");
        assert_eq!(tickets.outstanding(), 1);
    }
}
