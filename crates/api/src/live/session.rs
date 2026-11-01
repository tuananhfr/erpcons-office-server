//! One live connection: joins the room, forwards the room's events to the socket, applies what the
//! client sends, pings, notices silence, and re-reads the member's role on every access change and
//! periodically. Logs carry outcomes only, never ids, names or tickets.
use std::{net::IpAddr, time::Duration};

use axum::extract::ws::{CloseFrame, Message, WebSocket};
use office_documents::DocError;
use office_live::{
    ByeReason, CloseCode, Event, JoinRefused, Membership, MessageRate, Mode,
    protocol::{self, ClientMessage, LiveErrorCode},
};
use tokio::time::{Instant, MissedTickBehavior, interval, interval_at, timeout};
use tracing::{debug, warn};
use uuid::Uuid;

use super::{Viewer, viewer::mode_of};
use crate::AppState;

/// A connection that passed its ticket and access check, about to join.
pub(crate) struct Session {
    pub state: AppState,
    pub document: Uuid,
    pub viewer: Viewer,
    pub allowed: Mode,
    pub address: Option<IpAddr>,
}

/// How a session ends.
enum End {
    /// The server says `bye` with a reason, then closes.
    Bye(ByeReason),
    /// The client said `bye`.
    ClientBye,
    /// The connection is gone, or closed by the client.
    Gone,
}

pub(crate) async fn run(mut socket: WebSocket, session: Session) {
    let hub = session.state.live.clone();
    let joined = hub.join(
        session.document,
        session.viewer.participant(),
        session.allowed,
        session.address,
    );
    let (membership, mut events) = match joined {
        Ok(joined) => joined,
        Err(refused) => {
            let (code, close_code) = match refused {
                JoinRefused::RoomFull => (LiveErrorCode::RoomFull, CloseCode::Full),
                JoinRefused::AddressFull => (LiveErrorCode::TooManyConnections, CloseCode::Full),
                JoinRefused::ServerFull => (LiveErrorCode::ServerBusy, CloseCode::Full),
                JoinRefused::ShuttingDown => (LiveErrorCode::ServerBusy, CloseCode::GoingAway),
            };
            debug!(refused = ?refused, "live join refused");
            let grace = hub.config().idle_timeout;
            let _ = send(&mut socket, protocol::error(code), grace).await;
            close(&mut socket, close_code, grace).await;
            return;
        }
    };
    let config = hub.config().clone();
    let grace = config.idle_timeout;
    let mut rate = MessageRate::new(
        config.message_burst,
        config.messages_per_second,
        std::time::Instant::now(),
    );
    let mut last_heard = Instant::now();
    let mut last_ping = Instant::now();
    // Checks silence often enough that a dead client is noticed soon after the idle timeout.
    let mut tick = interval(config.ping_interval.min(config.idle_timeout) / 4);
    tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut recheck = interval_at(
        Instant::now() + config.recheck_interval,
        config.recheck_interval,
    );
    recheck.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let shutdown = hub.shutdown_token().clone();

    let end = loop {
        tokio::select! {
            frame = socket.recv() => {
                let Some(Ok(frame)) = frame else {
                    break End::Gone;
                };
                last_heard = Instant::now();
                if let Some(end) = on_frame(&mut socket, &membership, &mut rate, frame, grace).await {
                    break end;
                }
            }
            event = events.recv() => match event {
                Some(Event::Send(text)) => {
                    if send(&mut socket, text, grace).await.is_err() {
                        break End::Gone;
                    }
                }
                Some(Event::Close(reason)) => break End::Bye(reason),
                Some(Event::Recheck) => {
                    if let Some(end) = recheck_access(&session, &membership).await {
                        break end;
                    }
                }
                // The room dropped this member: it fell behind.
                None => break End::Bye(ByeReason::Overloaded),
            },
            _ = tick.tick() => {
                if last_heard.elapsed() >= config.idle_timeout {
                    break End::Bye(ByeReason::Timeout);
                }
                if last_ping.elapsed() >= config.ping_interval {
                    last_ping = Instant::now();
                    if send_frame(&mut socket, Message::Ping(Default::default()), grace).await.is_err() {
                        break End::Gone;
                    }
                }
            }
            _ = recheck.tick() => {
                if let Some(end) = recheck_access(&session, &membership).await {
                    break end;
                }
            }
            () = shutdown.cancelled() => break End::Bye(ByeReason::Shutdown),
        }
    };
    // Leave first, so the others see it even if the goodbye below cannot be delivered.
    drop(membership);
    match end {
        End::Bye(reason) => {
            debug!(reason = ?reason, "live session ended");
            if send(&mut socket, protocol::bye(reason), grace)
                .await
                .is_ok()
            {
                close(&mut socket, reason.close_code(), grace).await;
            }
        }
        End::ClientBye => close(&mut socket, CloseCode::Normal, grace).await,
        End::Gone => {}
    }
}

/// Applies one client frame; `Some` ends the session.
async fn on_frame(
    socket: &mut WebSocket,
    membership: &Membership,
    rate: &mut MessageRate,
    frame: Message,
    grace: Duration,
) -> Option<End> {
    let text = match frame {
        Message::Close(_) => return Some(End::Gone),
        Message::Pong(_) => return None,
        Message::Text(text) => Some(text),
        Message::Binary(_) | Message::Ping(_) => None,
    };
    if !rate.check(std::time::Instant::now()) {
        let _ = send(socket, protocol::error(LiveErrorCode::RateLimited), grace).await;
        return Some(End::Bye(ByeReason::RateLimited));
    }
    let text = text?;
    match protocol::parse(text.as_str()) {
        Ok(ClientMessage::Presence(state)) => {
            membership.set_state(state);
            None
        }
        Ok(ClientMessage::Bye) => Some(End::ClientBye),
        Err(code) => send(socket, protocol::error(code), grace)
            .await
            .err()
            .map(|()| End::Gone),
    }
}

/// Reads the member's role again; `Some` when it may no longer open the document.
async fn recheck_access(session: &Session, membership: &Membership) -> Option<End> {
    match session
        .viewer
        .access(&session.state.docs, session.document)
        .await
    {
        Ok(access) => {
            membership.set_allowed(mode_of(access));
            None
        }
        Err(DocError::NotFound | DocError::Forbidden) => Some(End::Bye(ByeReason::Revoked)),
        // A database hiccup must not throw everyone out; the next check decides.
        Err(e) => {
            warn!(error = %e, "live access re-check failed");
            None
        }
    }
}

async fn send(socket: &mut WebSocket, text: String, grace: Duration) -> Result<(), ()> {
    send_frame(socket, Message::Text(text.into()), grace).await
}

/// A client that stops reading must not hold the session forever.
async fn send_frame(socket: &mut WebSocket, frame: Message, grace: Duration) -> Result<(), ()> {
    match timeout(grace, socket.send(frame)).await {
        Ok(Ok(())) => Ok(()),
        _ => Err(()),
    }
}

/// How long a closing session waits for the client's own close frame.
const CLOSE_WAIT: Duration = Duration::from_secs(2);

async fn close(socket: &mut WebSocket, code: CloseCode, grace: Duration) {
    let frame = CloseFrame {
        code: code.code(),
        reason: "".into(),
    };
    if send_frame(socket, Message::Close(Some(frame)), grace)
        .await
        .is_err()
    {
        return;
    }
    // Dropping the socket at once could reset the connection before the client read our code.
    let _ = timeout(CLOSE_WAIT, async {
        while let Some(Ok(frame)) = socket.recv().await {
            if matches!(frame, Message::Close(_)) {
                break;
            }
        }
    })
    .await;
}
