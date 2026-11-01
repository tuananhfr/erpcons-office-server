use office_documents::{DocError, Documents, LinkHash, LiveAccess, LiveViewer, Role};
use office_identity::Actor;
use office_live::{Mode, Participant};
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

    pub(crate) fn participant(&self) -> Participant {
        match self {
            Self::Actor(actor) => Participant::User {
                actor_id: actor.actor_id.clone(),
                name: actor.display_name.clone(),
            },
            Self::Link(_) => Participant::Link,
        }
    }
}

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
}
