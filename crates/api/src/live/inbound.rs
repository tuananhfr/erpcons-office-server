//! What a client sends: size and rate checks, then the room. Rates are per connection: every frame
//! counts towards the message rate, except cursors within their own allowance; `region.update`
//! also spends a byte allowance and `region.replay` a request allowance.
use std::time::Instant;

use axum::extract::ws::Message;
use office_live::{
    CloseCode, Frame, LiveConfig, Membership, MessageRate,
    protocol::{self, ClientMessage, LiveErrorCode},
};

use super::session::End;

pub(super) struct Budgets {
    messages: MessageRate,
    cursors: MessageRate,
    region_bytes: MessageRate,
    replays: MessageRate,
}

impl Budgets {
    pub(super) fn new(config: &LiveConfig, now: Instant) -> Self {
        Self {
            messages: MessageRate::new(config.message_burst, config.messages_per_second, now),
            cursors: MessageRate::new(config.cursor_burst, config.cursors_per_second, now),
            region_bytes: MessageRate::new(
                config.region_bytes_burst,
                config.region_bytes_per_second,
                now,
            ),
            replays: MessageRate::new(config.replay_burst, config.replays_per_second, now),
        }
    }
}

/// What the session does next.
pub(super) enum Inbound {
    Continue,
    /// Answer `error` with this code; the connection stays.
    Refuse(LiveErrorCode),
    /// Answer `error` `RATE_LIMITED`, then `bye` and close.
    RateLimited,
    /// Send these straight to the socket, in order (a replay).
    Send(Vec<Frame>),
    End(End),
}

/// Applies one client frame.
pub(super) fn on_frame(
    membership: &Membership,
    budgets: &mut Budgets,
    config: &LiveConfig,
    frame: Message,
) -> Inbound {
    let now = Instant::now();
    let (parsed, bytes) = match frame {
        Message::Close(_) => return Inbound::End(End::Gone),
        // Pongs answer our pings, but unasked ones must not flood the session for free.
        Message::Ping(_) | Message::Pong(_) => {
            return if budgets.messages.check(now) {
                Inbound::Continue
            } else {
                Inbound::RateLimited
            };
        }
        Message::Text(text) => (protocol::parse(text.as_str()), text.len()),
        Message::Binary(bytes) => (Err(LiveErrorCode::InvalidMessage), bytes.len()),
    };
    let big = bytes > config.max_message_bytes;
    let message = match parsed {
        Ok(message) if big && !matches!(message, ClientMessage::RegionUpdate { .. }) => {
            return Inbound::End(End::Close(CloseCode::TooBig));
        }
        // A message this large that is not even valid is no smaller.
        Err(_) if big => return Inbound::End(End::Close(CloseCode::TooBig)),
        other => other,
    };
    let cursor = matches!(message, Ok(ClientMessage::Cursor { .. }));
    // Cursors within their allowance are free of the message rate; past it they count and are
    // not relayed.
    if !(cursor && budgets.cursors.check(now)) {
        if !budgets.messages.check(now) {
            return Inbound::RateLimited;
        }
        if cursor {
            return Inbound::Continue;
        }
    }
    let message = match message {
        Ok(message) => message,
        Err(code) => return Inbound::Refuse(code),
    };
    match message {
        ClientMessage::Presence(state) => membership.set_state(state),
        ClientMessage::Bye => return Inbound::End(End::ClientBye),
        ClientMessage::LockAcquire { region, cid } => membership.acquire_lock(&region, &cid),
        ClientMessage::LockRenew { region } => membership.renew_lock(&region),
        ClientMessage::LockRelease { region } => membership.release_lock(&region),
        ClientMessage::RegionUpdate {
            region,
            cid,
            payload,
        } => {
            if !budgets.region_bytes.take(now, bytes as f64) {
                return Inbound::RateLimited;
            }
            membership.update(&region, &cid, &payload);
        }
        ClientMessage::Cursor { region, range } => membership.cursor(&region, &range),
        ClientMessage::Replay { since } => {
            if !budgets.replays.check(now) {
                return Inbound::RateLimited;
            }
            return Inbound::Send(membership.replay(since));
        }
    }
    Inbound::Continue
}
