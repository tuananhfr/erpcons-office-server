use office_identity::Actor;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Read,
    Write,
    Trash,
    Restore,
}

/// What is being accessed, as loaded from the database.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resource<'a> {
    Document { owner_id: &'a str },
    Folder { owner_id: &'a str },
}

/// The only refusal: the caller learns nothing about whether the resource exists (spec §08).
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("not found")]
pub struct NotFound;

/// The single access decision of the service, deny by default. Phase 1: only the owner, for every
/// action. Phase 2 replaces the body with ACL checks; handlers keep calling this.
pub fn authorize(actor: &Actor, resource: Resource<'_>, action: Action) -> Result<(), NotFound> {
    let owner_id = match resource {
        Resource::Document { owner_id } | Resource::Folder { owner_id } => owner_id,
    };
    let is_owner = !actor.actor_id.is_empty() && actor.actor_id == owner_id;
    match action {
        Action::Read | Action::Write | Action::Trash | Action::Restore if is_owner => Ok(()),
        _ => Err(NotFound),
    }
}

#[cfg(test)]
mod tests {
    use time::OffsetDateTime;

    use super::*;

    const ACTIONS: [Action; 4] = [Action::Read, Action::Write, Action::Trash, Action::Restore];

    fn actor(id: &str) -> Actor {
        Actor {
            actor_id: id.into(),
            display_name: "An".into(),
            expires_at: OffsetDateTime::UNIX_EPOCH,
        }
    }

    fn resources(owner_id: &str) -> [Resource<'_>; 2] {
        [
            Resource::Document { owner_id },
            Resource::Folder { owner_id },
        ]
    }

    #[test]
    fn owner_may_do_everything() {
        for resource in resources("usr_a") {
            for action in ACTIONS {
                assert_eq!(authorize(&actor("usr_a"), resource, action), Ok(()));
            }
        }
    }

    #[test]
    fn anyone_else_gets_not_found() {
        for resource in resources("usr_a") {
            for action in ACTIONS {
                assert_eq!(authorize(&actor("usr_b"), resource, action), Err(NotFound));
                assert_eq!(authorize(&actor("USR_A"), resource, action), Err(NotFound));
            }
        }
    }

    #[test]
    fn empty_ids_never_match() {
        for resource in resources("") {
            for action in ACTIONS {
                assert_eq!(authorize(&actor(""), resource, action), Err(NotFound));
            }
        }
    }
}
