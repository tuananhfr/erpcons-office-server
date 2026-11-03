//! Entering a document's live room: a one-time ticket (PI token, or the public link's token), then
//! the WebSocket opened with it. The ticket is the only secret in the URL: short-lived, spent on
//! first use, bound to the document and the client address. Access is decided again at the upgrade.
use std::{net::IpAddr, time::Instant};

use axum::{
    Extension, Json,
    extract::{
        State,
        ws::{WebSocketUpgrade, rejection::WebSocketUpgradeRejection},
    },
    response::Response,
};
use office_documents::LiveViewer;
use office_live::TicketError;
use serde::Deserialize;
use uuid::Uuid;

use super::link_or_actor::LinkOrActor;
use crate::{
    ApiError, AppState, Authenticated, ErrorCode, RequestId,
    dto::LiveTicketDto,
    extract::{ApiPath, ApiQuery},
    live::{self, Session, Viewer, mode_of},
    rate_limit::ClientKey,
};

fn address(client: Option<Extension<ClientKey>>) -> Option<IpAddr> {
    client.and_then(|Extension(ClientKey(ip))| ip)
}

pub(crate) async fn ticket(
    State(state): State<AppState>,
    Extension(rid): Extension<RequestId>,
    client: Option<Extension<ClientKey>>,
    ApiPath(id): ApiPath<Uuid>,
    caller: LinkOrActor,
) -> Result<Json<LiveTicketDto>, ApiError> {
    let (viewer, link_owner) = match caller {
        LinkOrActor::Actor(Authenticated(actor)) => {
            state
                .docs
                .live_access(id, LiveViewer::Actor(&actor))
                .await?;
            (Viewer::Actor(actor), None)
        }
        LinkOrActor::Link(hash) => {
            let (_, owner) = state.docs.live_link_access(id, &hash, rid.as_str()).await?;
            (Viewer::Link(hash), Some(owner))
        }
    };
    let address = address(client);
    let ticket = state
        .tickets
        .issue(
            id,
            address,
            &viewer.quota_key(address),
            link_owner.as_deref(),
            viewer,
            Instant::now(),
        )
        .map_err(|e| match e {
            // Tickets are spent or lapse within the TTL, freeing room.
            TicketError::Full => ApiError::retry_after(ErrorCode::Unavailable, 1),
            TicketError::NoAddress | TicketError::Random => ApiError::from(ErrorCode::Unavailable),
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
    let allowed = mode_of(access);
    let max = state.live.config().frame_limit(allowed);
    let session = Session {
        state,
        document: id,
        viewer,
        allowed,
        address,
    };
    Ok(upgrade
        .max_message_size(max)
        .max_frame_size(max)
        .on_upgrade(move |socket| live::run(socket, session)))
}
