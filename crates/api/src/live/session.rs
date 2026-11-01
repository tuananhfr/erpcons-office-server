//! One live connection: joins the room, forwards the room's events to the socket, applies what the
//! client sends, pings, notices silence, sweeps lapsed locks, and re-reads the member's role on
//! every access change and periodically. Logs carry outcomes only, never ids, names, regions,
//! payloads or tickets.
use std::{net::IpAddr, time::Duration};

use axum::extract::ws::{CloseFrame, Message, WebSocket};
use office_documents::DocError;
use office_live::{
    ByeReason, CloseCode, Event, Frame, JoinRefused, Membership, Mode,
    protocol::{self, LiveErrorCode},
};
use tokio::{
    sync::mpsc::{Receiver, error::TryRecvError},
    time::{Instant, MissedTickBehavior, interval, interval_at, timeout},
};
use tracing::{debug, warn};
use uuid::Uuid;

use super::{
    Viewer,
    inbound::{self, Budgets, Inbound},
    viewer::mode_of,
};
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
pub(super) enum End {
    /// The server says `bye` with a reason, then closes.
    Bye(ByeReason),
    /// The server closes with this code and no `bye`.
    Close(CloseCode),
    /// The client said `bye`.
    ClientBye,
    /// The transport refused a frame over the size limit; closes with 1009.
    Oversized,
    /// The connection is gone, or closed by the client.
    Gone,
}

/// Room events held back while a replay is written; more than this and the client is too slow.
const MAX_HELD_EVENTS: usize = 4096;

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
                JoinRefused::AddressFull | JoinRefused::ActorFull => {
                    (LiveErrorCode::TooManyConnections, CloseCode::Full)
                }
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
    let mut budgets = Budgets::new(&config, std::time::Instant::now());
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
    // Nothing else keeps time for the room: a lock lapses at most a fifth of its lease late.
    let mut sweep = interval((config.lock_ttl / 5).max(Duration::from_millis(10)));
    sweep.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let shutdown = hub.shutdown_token().clone();

    let end = 'session: {
        // A change committed between the check at the upgrade and the join reached no room.
        if let Some(end) = recheck_access(&session, &membership).await {
            break 'session end;
        }
        loop {
            tokio::select! {
                frame = socket.recv() => {
                    let frame = match frame {
                        Some(Ok(frame)) => frame,
                        Some(Err(e)) if over_limit(&e) => break End::Oversized,
                        _ => break End::Gone,
                    };
                    last_heard = Instant::now();
                    let inbound = inbound::on_frame(&membership, &mut budgets, &config, frame);
                    if let Some(end) = answer(&mut socket, &session, &membership, &mut events, inbound, grace).await {
                        break end;
                    }
                }
                event = events.recv() => {
                    // `None`: the room dropped this member, it fell behind.
                    let Some(event) = event else {
                        break End::Bye(ByeReason::Overloaded);
                    };
                    if let Some(end) = on_event(&mut socket, &session, &membership, event, grace).await {
                        break end;
                    }
                }
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
                _ = sweep.tick() => membership.expire_locks(),
                _ = recheck.tick() => {
                    if let Some(end) = recheck_access(&session, &membership).await {
                        break end;
                    }
                }
                () = shutdown.cancelled() => break End::Bye(ByeReason::Shutdown),
            }
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
        End::Close(code) => {
            debug!(code = code.code(), "live session closed");
            close(&mut socket, code, grace).await;
        }
        End::ClientBye => close(&mut socket, CloseCode::Normal, grace).await,
        End::Oversized => {
            debug!(code = CloseCode::TooBig.code(), "live session closed");
            close(&mut socket, CloseCode::TooBig, grace).await;
            // The socket reads nothing more, so the client's close never comes; dropping it with
            // the rest of the frame unread resets the connection, which may discard our code.
            tokio::time::sleep(OVERSIZED_LINGER).await;
        }
        End::Gone => {}
    }
}

/// Whether the transport refused a frame over the connection's size limit; it sends no close
/// itself, and the socket can still say 1009.
fn over_limit(error: &axum::Error) -> bool {
    matches!(
        std::error::Error::source(error).and_then(|e| e.downcast_ref::<tungstenite::Error>()),
        Some(tungstenite::Error::Capacity(_))
    )
}

/// Carries out what a client frame asked for; `Some` ends the session.
async fn answer(
    socket: &mut WebSocket,
    session: &Session,
    membership: &Membership,
    events: &mut Receiver<Event>,
    inbound: Inbound,
    grace: Duration,
) -> Option<End> {
    match inbound {
        Inbound::Continue => None,
        Inbound::Refuse(code) => send(socket, protocol::error(code), grace)
            .await
            .err()
            .map(|()| End::Gone),
        Inbound::RateLimited => {
            let _ = send(socket, protocol::error(LiveErrorCode::RateLimited), grace).await;
            Some(End::Bye(ByeReason::RateLimited))
        }
        Inbound::Send(messages) => {
            let held = match write_through(socket, events, messages, grace).await {
                Ok(held) => held,
                Err(end) => return Some(end),
            };
            for event in held {
                if let Some(end) = on_event(socket, session, membership, event, grace).await {
                    return Some(end);
                }
            }
            None
        }
        Inbound::End(end) => Some(end),
    }
}

/// Writes `messages` (a replay, up to the room's whole log) straight to the socket, holding back
/// the room's events meanwhile so the queue does not overflow; the held ones come after, in order.
async fn write_through(
    socket: &mut WebSocket,
    events: &mut Receiver<Event>,
    messages: Vec<Frame>,
    grace: Duration,
) -> Result<Vec<Event>, End> {
    let mut held = Vec::new();
    for text in messages {
        send(socket, text.to_text(), grace)
            .await
            .map_err(|()| End::Gone)?;
        loop {
            match events.try_recv() {
                Ok(event) => held.push(event),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return Err(End::Bye(ByeReason::Overloaded)),
            }
        }
        if held.len() > MAX_HELD_EVENTS {
            return Err(End::Bye(ByeReason::Overloaded));
        }
    }
    Ok(held)
}

/// Carries out one room event; `Some` ends the session.
async fn on_event(
    socket: &mut WebSocket,
    session: &Session,
    membership: &Membership,
    event: Event,
    grace: Duration,
) -> Option<End> {
    match event {
        Event::Send(frame) => send(socket, frame.to_text(), grace)
            .await
            .err()
            .map(|()| End::Gone),
        Event::Close(reason) => Some(End::Bye(reason)),
        Event::Recheck => {
            membership.recheck_started();
            recheck_access(session, membership).await
        }
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
/// How long the close frame gets to reach a client whose oversized frame is left unread.
const OVERSIZED_LINGER: Duration = Duration::from_millis(500);

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
