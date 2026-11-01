use std::time::Duration;

/// Limits and timings of the live channel (plan-next §2.1, §5).
#[derive(Clone, Debug, PartialEq)]
pub struct LiveConfig {
    /// A ticket opens one connection, this soon after it was issued.
    pub ticket_ttl: Duration,
    /// Unredeemed tickets held at once, server-wide.
    pub max_tickets: usize,
    /// Unredeemed tickets of one holder (account or public link), and from one client address; a
    /// new one past either replaces that key's oldest, so nobody can fill the store for everyone.
    pub max_tickets_per_holder: usize,
    pub max_tickets_per_address: usize,
    /// Largest message or frame a client may send.
    pub max_message_bytes: usize,
    /// Messages a connection may send at once, then refilled at `messages_per_second`.
    pub message_burst: u32,
    pub messages_per_second: f64,
    pub max_per_document: usize,
    /// Members past this who may edit join in view mode, told so, until a slot frees.
    pub max_editors: usize,
    /// Connections from one client address (an IPv6 client by its /64).
    pub max_per_address: usize,
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
            max_message_bytes: 16 * 1024,
            message_burst: 20,
            messages_per_second: 10.0,
            max_per_document: 50,
            max_editors: 10,
            // Offices share one public address behind NAT.
            max_per_address: 100,
            max_connections: 5_000,
            ping_interval: Duration::from_secs(20),
            idle_timeout: Duration::from_secs(30),
            recheck_interval: Duration::from_secs(60),
            outbox: 64,
        }
    }
}
