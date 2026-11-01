use std::time::Duration;

use crate::Mode;

/// Limits and timings of the live channel (plan-next §2.1, §5).
#[derive(Clone, Debug, PartialEq)]
pub struct LiveConfig {
    /// A ticket opens one connection, this soon after it was issued.
    pub ticket_ttl: Duration,
    /// Unredeemed tickets held at once, server-wide.
    pub max_tickets: usize,
    /// Unredeemed tickets of one holder (an account, or a public link from one client network); a
    /// new one past this replaces the holder's own oldest.
    pub max_tickets_per_holder: usize,
    /// Unredeemed tickets from one client network (an IPv6 client by its /56; one holder a quarter
    /// of it), and through the public links of one owner's documents (one network an eighth).
    /// Shared with others: past either the heaviest holder there loses its oldest ticket.
    pub max_tickets_per_address: usize,
    pub max_link_tickets_per_owner: usize,
    /// Largest message or frame a client may send, `region.update` aside.
    pub max_message_bytes: usize,
    /// Largest `region.update` message; only connections that joined able to edit take frames
    /// this large.
    pub max_region_message_bytes: usize,
    /// Bytes of `region.update` a connection may send at once, then refilled per second.
    pub region_bytes_burst: u32,
    pub region_bytes_per_second: f64,
    /// Cursor messages a connection may send at once, then per second, without counting towards
    /// the message rate; past that each counts and is not relayed.
    pub cursor_burst: u32,
    pub cursors_per_second: f64,
    /// `region.replay` requests a connection may send at once, then per second.
    pub replay_burst: u32,
    pub replays_per_second: f64,
    /// A region lock lapses this long after it was taken or last renewed.
    pub lock_ttl: Duration,
    pub max_locks_per_member: usize,
    /// Bytes of `region.update` a room keeps for replay since the last save; past this the oldest
    /// go and the leader is asked to save.
    pub max_log_bytes: usize,
    /// Client message ids a room remembers, so a resent update is acknowledged, not applied twice.
    pub max_acked_cids: usize,
    /// What all rooms' logs and remembered ids may take on the node; past it the largest logs
    /// shed their oldest updates and their leaders are asked to save.
    pub max_total_log_bytes: usize,
    /// Messages a connection may send at once, then refilled at `messages_per_second`.
    pub message_burst: u32,
    pub messages_per_second: f64,
    pub max_per_document: usize,
    /// Of those, members who came through the public link; the rest stay for people it is shared
    /// with.
    pub max_links_per_document: usize,
    /// Members past this who may edit join in view mode, told so, until a slot frees.
    pub max_editors: usize,
    /// Connections from one client network (an IPv6 client by its /56).
    pub max_per_address: usize,
    /// Connections of one account, over all documents.
    pub max_per_actor: usize,
    pub max_connections: usize,
    pub ping_interval: Duration,
    /// Nothing heard for this long and the member has left.
    pub idle_timeout: Duration,
    /// Backstop of the access re-check; changes made in this process are pushed at once.
    pub recheck_interval: Duration,
    /// Events queued for one connection; a client that falls this far behind is dropped.
    pub outbox: usize,
}

impl Default for LiveConfig {
    fn default() -> Self {
        Self {
            ticket_ttl: Duration::from_secs(30),
            max_tickets: 10_000,
            // Tickets are spent within moments of their issue; these bound only abuse.
            max_tickets_per_holder: 20,
            max_tickets_per_address: 100,
            max_link_tickets_per_owner: 300,
            max_message_bytes: 16 * 1024,
            max_region_message_bytes: 1024 * 1024,
            region_bytes_burst: 4 * 1024 * 1024,
            region_bytes_per_second: 1024.0 * 1024.0,
            cursor_burst: 20,
            cursors_per_second: 20.0,
            replay_burst: 3,
            replays_per_second: 0.2,
            lock_ttl: Duration::from_secs(5),
            max_locks_per_member: 64,
            max_log_bytes: 8 * 1024 * 1024,
            max_acked_cids: 4096,
            max_total_log_bytes: 256 * 1024 * 1024,
            message_burst: 20,
            messages_per_second: 10.0,
            max_per_document: 50,
            max_links_per_document: 30,
            max_editors: 10,
            // Offices share one public address behind NAT.
            max_per_address: 100,
            max_per_actor: 20,
            max_connections: 5_000,
            ping_interval: Duration::from_secs(20),
            idle_timeout: Duration::from_secs(30),
            recheck_interval: Duration::from_secs(60),
            outbox: 64,
        }
    }
}

impl LiveConfig {
    /// Largest message a connection that joined with `allowed` takes: only a would-be editor may
    /// send `region.update`, so nobody else gets frames that large.
    pub fn frame_limit(&self, allowed: Mode) -> usize {
        if allowed == Mode::Edit {
            self.max_region_message_bytes.max(self.max_message_bytes)
        } else {
            self.max_message_bytes
        }
    }
}
