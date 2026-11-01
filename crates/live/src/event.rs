use crate::ByeReason;

/// What the room asks a session to do, in order.
pub enum Event {
    /// A protocol message, already serialized.
    Send(String),
    /// Send `bye` with this reason, then close.
    Close(ByeReason),
    /// Access to the document may have changed: read the member's role again now.
    Recheck,
}
