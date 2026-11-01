use crate::{ByeReason, Frame};

/// What the room asks a session to do, in order.
pub enum Event {
    /// A protocol message, shared by every member it goes to.
    Send(Frame),
    /// Send `bye` with this reason, then close.
    Close(ByeReason),
    /// Access to the document may have changed: read the member's role again now. At most one is
    /// queued per member until the session calls [`crate::Membership::recheck_started`].
    Recheck,
}
