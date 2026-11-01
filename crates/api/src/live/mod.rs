//! The live channel's HTTP side: who a ticket was issued to, the bridge that lets the document
//! service reach the rooms, and the WebSocket session.
mod gate;
mod session;
mod viewer;

pub(crate) use gate::LiveGate;
pub(crate) use session::{Session, run};
pub(crate) use viewer::{Viewer, mode_of};
