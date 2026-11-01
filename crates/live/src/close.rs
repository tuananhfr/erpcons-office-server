use serde::Serialize;

/// WebSocket close codes of the live channel; `4xxx` are ours (RFC 6455 §7.4.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloseCode {
    /// The client said `bye`.
    Normal,
    /// The server is shutting down; reconnect later.
    GoingAway,
    /// Nothing heard for the idle timeout.
    Timeout,
    /// The caller may no longer open the document.
    Revoked,
    /// Too many messages, or a message the protocol does not allow at this rate.
    Policy,
    /// The client did not keep up with its events.
    Overloaded,
    /// Room, address or server full.
    Full,
}

impl CloseCode {
    pub fn code(self) -> u16 {
        match self {
            Self::Normal => 1000,
            Self::GoingAway => 1001,
            Self::Timeout => 4000,
            Self::Revoked => 4003,
            Self::Policy => 4008,
            Self::Overloaded => 4013,
            Self::Full => 4029,
        }
    }
}

/// Why the server ends a session, sent in `bye` just before the close frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ByeReason {
    Revoked,
    Timeout,
    Shutdown,
    RateLimited,
    Overloaded,
}

impl ByeReason {
    pub fn close_code(self) -> CloseCode {
        match self {
            Self::Revoked => CloseCode::Revoked,
            Self::Timeout => CloseCode::Timeout,
            Self::Shutdown => CloseCode::GoingAway,
            Self::RateLimited => CloseCode::Policy,
            Self::Overloaded => CloseCode::Overloaded,
        }
    }
}
