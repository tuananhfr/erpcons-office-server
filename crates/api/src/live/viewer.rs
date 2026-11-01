use std::net::IpAddr;

use office_documents::{DocError, Documents, LinkHash, LiveAccess, LiveViewer, Role};
use office_identity::Actor;
use office_live::{Mode, Participant, quota_network};
use uuid::Uuid;

/// Whom a ticket was issued to; checked again from the database at join and on every re-check.
/// No `Debug`: holds the display name.
#[derive(Clone)]
pub(crate) enum Viewer {
    Actor(Actor),
    Link(LinkHash),
}

impl Viewer {
    pub(crate) async fn access(&self, docs: &Documents, id: Uuid) -> Result<LiveAccess, DocError> {
        let viewer = match self {
            Self::Actor(actor) => LiveViewer::Actor(actor),
            Self::Link(hash) => LiveViewer::Link(hash),
        };
        docs.live_access(id, viewer).await
    }

    /// Names the holder for the ticket quota: the account, or the link from this client network.
    /// Everyone holding a public link shares its token; by the link alone, one could push the
    /// others' tickets out.
    pub(crate) fn quota_key(&self, address: Option<IpAddr>) -> String {
        match self {
            Self::Actor(actor) => format!("actor:{}", actor.actor_id),
            Self::Link(hash) => match quota_network(address) {
                Some(ip) => format!("link:{}:{ip}", hash.as_str()),
                None => format!("link:{}", hash.as_str()),
            },
        }
    }

    pub(crate) fn participant(&self) -> Participant {
        match self {
            Self::Actor(actor) => Participant::User {
                actor_id: actor.actor_id.clone(),
                name: actor.display_name.chars().take(MAX_NAME_CHARS).collect(),
            },
            Self::Link(_) => Participant::Link,
        }
    }
}

/// A display name as the room shows it: every presence carries every name, so a long one would
/// multiply what each change sends.
const MAX_NAME_CHARS: usize = 64;

/// The mode a role allows in the room; a trashed document (its owner's alone) is read-only.
pub(crate) fn mode_of(access: LiveAccess) -> Mode {
    match access.role {
        _ if access.trashed => Mode::View,
        Role::Owner | Role::Editor => Mode::Edit,
        Role::Commenter => Mode::Comment,
        Role::Viewer | Role::Link => Mode::View,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modes_follow_the_role_and_the_trash() {
        let mode = |role, trashed| mode_of(LiveAccess { role, trashed });
        assert_eq!(mode(Role::Owner, false), Mode::Edit);
        assert_eq!(mode(Role::Editor, false), Mode::Edit);
        assert_eq!(mode(Role::Commenter, false), Mode::Comment);
        assert_eq!(mode(Role::Viewer, false), Mode::View);
        assert_eq!(mode(Role::Link, false), Mode::View);
        assert_eq!(mode(Role::Owner, true), Mode::View);
    }

    #[test]
    fn a_room_shows_at_most_64_characters_of_a_name() {
        let actor = Actor {
            actor_id: "usr_1".into(),
            display_name: "Ă".repeat(300),
            email: office_identity::EmailClaim::Unknown,
            expires_at: time::OffsetDateTime::UNIX_EPOCH,
        };
        let Participant::User { name, .. } = Viewer::Actor(actor).participant() else {
            panic!("a user");
        };
        assert_eq!(name, "Ă".repeat(64));
    }
}
