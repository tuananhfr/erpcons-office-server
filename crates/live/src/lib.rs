//! Live rooms of the online tier (plan-next §2.1): one in-memory room per open document with its
//! members, their modes and presence, the leader whose saves count, the region-lock table M4 fills,
//! and the one-time tickets that open a connection. Transport-agnostic: a session pumps [`Event`]s
//! to its socket and reports what the client sends.
mod close;
mod config;
mod event;
mod hub;
mod locks;
mod member;
mod membership;
mod mode;
mod network;
mod palette;
pub mod protocol;
mod rate;
mod room;
mod tickets;

pub use close::{ByeReason, CloseCode};
pub use config::LiveConfig;
pub use event::Event;
pub use hub::{Hub, JoinRefused};
pub use locks::LockTable;
pub use member::{MemberId, Participant, PresenceState};
pub use membership::Membership;
pub use mode::{Mode, Notice};
pub use network::quota_network;
pub use rate::MessageRate;
pub use tickets::{TicketError, Tickets};
