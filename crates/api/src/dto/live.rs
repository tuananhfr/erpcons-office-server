use serde::Serialize;

/// `createLiveTicket`. No `Debug`: the ticket opens the room.
#[derive(Serialize)]
pub(crate) struct LiveTicketDto {
    pub ticket: String,
    /// Seconds the ticket stays valid.
    pub expires_in: u64,
}
