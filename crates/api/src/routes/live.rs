//! Entering a document's live room: a one-time ticket (PI token, or the public link's token), then
//! the WebSocket opened with it. The ticket is the only secret in the URL: short-lived, spent on
//! first use, bound to the document and the client address. Access is decided again at the upgrade.
use std::{net::IpAddr, time::Instant};

use axum::{
    Extension, Json,
    extract::{
        FromRequestParts, State,
        ws::{WebSocketUpgrade, rejection::WebSocketUpgradeRejection},
    },
    http::request::Parts,
    response::Response,
};
use office_documents::{LinkHash, LiveViewer};
use office_live::TicketError;
use serde::Deserialize;
use uuid::Uuid;

use super::public_links;
use crate::{
    ApiError, AppState, Authenticated, ErrorCode, RequestId,
    dto::LiveTicketDto,
    extract::{ApiPath, ApiQuery},
    live::{self, Session, Viewer, mode_of},
    rate_limit::ClientKey,
};

/// Who asks for a ticket: the link holder when `X-Link-Token` is sent, else the bearer's actor.
pub(crate) enum TicketCaller {
    Actor(Authenticated),
    Link(LinkHash),
}

impl FromRequestParts<AppState> for TicketCaller {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        if parts.headers.contains_key(public_links::TOKEN_HEADER) {
            public_links::check_rate(state, parts.extensions.get::<ClientKey>().copied())?;
            let token = public_links::token(&parts.headers)?;
            // A malformed token is as unknown as a wrong one.
            let hash = LinkHash::of(token).ok_or(ErrorCode::NotFound)?;
            return Ok(Self::Link(hash));
        }
        Authenticated::from_request_parts(parts, state)
            .await
            .map(Self::Actor)
    }
}

fn address(client: Option<Extension<ClientKey>>) -> Option<IpAddr> {
    client.and_then(|Extension(ClientKey(ip))| ip)
}

pub(crate) async fn ticket(
    State(state): State<AppState>,
    Extension(rid): Extension<RequestId>,
    client: Option<Extension<ClientKey>>,
    ApiPath(id): ApiPath<Uuid>,
    caller: TicketCaller,
) -> Result<Json<LiveTicketDto>, ApiError> {
    let viewer = match caller {
        TicketCaller::Actor(Authenticated(actor)) => {
            state
                .docs
                .live_access(id, LiveViewer::Actor(&actor))
                .await?;
            Viewer::Actor(actor)
        }
        TicketCaller::Link(hash) => {
            state.docs.live_link_access(id, &hash, rid.as_str()).await?;
            Viewer::Link(hash)
        }
    };
    let address = address(client);
    let ticket = state
        .tickets
        .issue(
            id,
            address,
            &viewer.quota_key(address),
            viewer,
            Instant::now(),
        )
        .map_err(|e| match e {
            TicketError::Full => ApiError::retry_after(ErrorCode::Unavailable, 1),
            TicketError::Random => ErrorCode::Unavailable.into(),
        })?;
    Ok(Json(LiveTicketDto {
        ticket,
        expires_in: state.tickets.ttl().as_secs(),
    }))
}

#[derive(Deserialize)]
pub(crate) struct LiveQuery {
    ticket: Option<String>,
}

pub(crate) async fn open(
    State(state): State<AppState>,
    client: Option<Extension<ClientKey>>,
    ApiPath(id): ApiPath<Uuid>,
    ApiQuery(query): ApiQuery<LiveQuery>,
    upgrade: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> Result<Response, ApiError> {
    // Checked first, so a plain GET does not spend the ticket.
    let upgrade = upgrade.map_err(|_| ErrorCode::InvalidRequest)?;
    let ticket = query.ticket.ok_or(ErrorCode::Unauthenticated)?;
    let address = address(client);
    let viewer = state
        .tickets
        .redeem(&ticket, id, address, Instant::now())
        .ok_or(ErrorCode::Unauthenticated)?;
    let access = viewer.access(&state.docs, id).await?;
    let max = state.live.config().max_message_bytes;
    let session = Session {
        state,
        document: id,
        viewer,
        allowed: mode_of(access),
        address,
    };
    Ok(upgrade
        .max_message_size(max)
        .max_frame_size(max)
        .on_upgrade(move |socket| live::run(socket, session)))
}
