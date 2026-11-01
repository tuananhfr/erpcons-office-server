use std::{fmt, net::IpAddr};

use serde::{Deserialize, Serialize, Serializer};
use tokio::sync::mpsc;

use crate::{Event, Mode, Notice};

/// A member's id, never reused on this node (a room emptied and opened again included) and never
/// an actor id: a session that outlived its place must not act on whoever came after it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MemberId(u64);

impl MemberId {
    pub(crate) fn new(n: u64) -> Self {
        Self(n)
    }
}

impl fmt::Display for MemberId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "m{}", self.0)
    }
}

impl Serialize for MemberId {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

/// Who a member is. No `Debug`: holds the display name.
#[derive(Clone)]
pub enum Participant {
    User {
        actor_id: String,
        name: String,
    },
    /// Someone holding the public view link; anonymous to the room.
    Link,
}

impl Participant {
    pub fn actor_id(&self) -> Option<&str> {
        match self {
            Self::User { actor_id, .. } => Some(actor_id),
            Self::Link => None,
        }
    }

    pub(crate) fn kind(&self) -> &'static str {
        match self {
            Self::User { .. } => "user",
            Self::Link => "link",
        }
    }

    /// `None` for link holders and for a user whose name PI did not give.
    pub(crate) fn name(&self) -> Option<&str> {
        match self {
            Self::User { name, .. } if !name.trim().is_empty() => Some(name),
            _ => None,
        }
    }
}

/// What the member's client reports about itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PresenceState {
    Active,
    Idle,
}

pub(crate) struct Member {
    pub participant: Participant,
    pub colour: &'static str,
    /// What the member's role allows.
    pub allowed: Mode,
    /// Holds one of the room's editor slots.
    pub slot: bool,
    pub state: PresenceState,
    /// An `Event::Recheck` is queued and not yet taken up.
    pub recheck_queued: bool,
    pub address: Option<IpAddr>,
    pub sender: mpsc::Sender<Event>,
}

impl Member {
    pub fn mode(&self) -> Mode {
        if self.capped() {
            Mode::View
        } else {
            self.allowed
        }
    }

    pub fn notice(&self) -> Option<Notice> {
        self.capped().then_some(Notice::EditorLimit)
    }

    fn capped(&self) -> bool {
        self.allowed == Mode::Edit && !self.slot
    }
}
