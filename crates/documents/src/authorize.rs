use office_identity::Actor;

/// The caller's role on a document (plan v2 §3, the Google Docs model).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Owner,
    Editor,
    Commenter,
    Viewer,
    /// Anyone holding the public view link, signed in or not.
    Link,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Owner => "owner",
            Self::Editor => "editor",
            Self::Commenter => "commenter",
            Self::Viewer => "viewer",
            Self::Link => "link",
        }
    }

    /// A role a permission can grant (`document_acl.role`).
    pub fn parse_grant(s: &str) -> Option<Self> {
        match s {
            "editor" => Some(Self::Editor),
            "commenter" => Some(Self::Commenter),
            "viewer" => Some(Self::Viewer),
            _ => None,
        }
    }

    /// The owner and editors always may; the others only when the owner allows it.
    pub fn can_download(self, viewers_can_download: bool) -> bool {
        matches!(self, Self::Owner | Self::Editor) || viewers_can_download
    }

    /// May add, change and remove permissions and the link (of a document not in the trash).
    pub fn can_share(self, editors_can_share: bool) -> bool {
        document_allows(self, Action::Share, editors_can_share)
    }

    /// The role of `actor` on a document of `owner_id`, given the permission it holds there.
    pub fn of(actor: Option<&Actor>, owner_id: &str, grant: Option<Self>) -> Option<Self> {
        let is_owner = actor.is_some_and(|a| !a.actor_id.is_empty() && a.actor_id == owner_id);
        if is_owner {
            Some(Self::Owner)
        } else {
            grant.filter(|g| *g != Self::Owner)
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Open: metadata and current bytes.
    Read,
    /// The version history and old versions' bytes.
    ReadVersions,
    /// Save content, restore a version.
    Write,
    /// Delete an old version for good: the history and the quota are the owner's.
    DeleteVersion,
    Rename,
    /// Into another folder: folders are the owner's.
    Move,
    Trash,
    Restore,
    /// Delete for good, from the trash.
    Purge,
    /// See the permissions (commenters and viewers only their own).
    ListPermissions,
    /// Add, change and remove permissions, turn the link on and off.
    Share,
    /// The two switches of the share settings.
    ChangeShareSettings,
    /// Remove one's own permission.
    Leave,
}

/// What is being accessed, as loaded from the database for this request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resource<'a> {
    Document {
        /// From [`Role::of`]; `None` when the caller has none.
        role: Option<Role>,
        trashed: bool,
        editors_can_share: bool,
    },
    /// Folders are never shared.
    Folder { owner_id: &'a str },
}

/// Why access was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Denied {
    /// The caller learns nothing about whether the resource exists (spec §08).
    #[error("not found")]
    NotFound,
    /// The caller can see the document, but its role does not allow this.
    #[error("forbidden")]
    Forbidden,
}

/// The single access decision of the service, deny by default (plan v2 §3).
pub fn authorize(
    actor: Option<&Actor>,
    resource: Resource<'_>,
    action: Action,
) -> Result<(), Denied> {
    match resource {
        Resource::Folder { owner_id } => {
            let is_owner = actor.is_some_and(|a| !a.actor_id.is_empty() && a.actor_id == owner_id);
            if is_owner {
                Ok(())
            } else {
                Err(Denied::NotFound)
            }
        }
        Resource::Document {
            role,
            trashed,
            editors_can_share,
        } => {
            let role = role.ok_or(Denied::NotFound)?;
            // The trash is the owner's: shared people and link holders stop seeing the document.
            if trashed && role != Role::Owner {
                return Err(Denied::NotFound);
            }
            if document_allows(role, action, editors_can_share) {
                Ok(())
            } else {
                Err(Denied::Forbidden)
            }
        }
    }
}

fn document_allows(role: Role, action: Action, editors_can_share: bool) -> bool {
    use Role::{Commenter, Editor, Link, Owner, Viewer};
    match action {
        Action::Read => true,
        Action::ReadVersions | Action::Write | Action::Rename => matches!(role, Owner | Editor),
        Action::Move
        | Action::Trash
        | Action::Restore
        | Action::Purge
        | Action::DeleteVersion
        | Action::ChangeShareSettings => role == Owner,
        Action::ListPermissions => role != Link,
        Action::Share => role == Owner || (role == Editor && editors_can_share),
        Action::Leave => matches!(role, Editor | Commenter | Viewer),
    }
}

#[cfg(test)]
mod tests {
    use time::OffsetDateTime;

    use super::*;

    const ACTIONS: [Action; 13] = [
        Action::Read,
        Action::ReadVersions,
        Action::Write,
        Action::DeleteVersion,
        Action::Rename,
        Action::Move,
        Action::Trash,
        Action::Restore,
        Action::Purge,
        Action::ListPermissions,
        Action::Share,
        Action::ChangeShareSettings,
        Action::Leave,
    ];

    fn actor(id: &str) -> Actor {
        Actor {
            actor_id: id.into(),
            display_name: "An".into(),
            email: office_identity::EmailClaim::Unknown,
            expires_at: OffsetDateTime::UNIX_EPOCH,
        }
    }

    fn doc(role: Option<Role>, trashed: bool, editors_can_share: bool) -> Resource<'static> {
        Resource::Document {
            role,
            trashed,
            editors_can_share,
        }
    }

    /// The plan's table, one row per action: owner, editor, commenter, viewer, link.
    fn expected(action: Action, editors_can_share: bool) -> [bool; 5] {
        match action {
            Action::Read => [true; 5],
            Action::ReadVersions | Action::Write | Action::Rename => {
                [true, true, false, false, false]
            }
            Action::Move
            | Action::Trash
            | Action::Restore
            | Action::Purge
            | Action::DeleteVersion
            | Action::ChangeShareSettings => [true, false, false, false, false],
            Action::ListPermissions => [true, true, true, true, false],
            Action::Share => [true, editors_can_share, false, false, false],
            Action::Leave => [false, true, true, true, false],
        }
    }

    #[test]
    fn the_role_matrix_follows_the_plan() {
        let roles = [
            Role::Owner,
            Role::Editor,
            Role::Commenter,
            Role::Viewer,
            Role::Link,
        ];
        for editors_can_share in [true, false] {
            for action in ACTIONS {
                let want = expected(action, editors_can_share);
                for (role, allowed) in roles.into_iter().zip(want) {
                    let got = authorize(None, doc(Some(role), false, editors_can_share), action);
                    let want = if allowed {
                        Ok(())
                    } else {
                        Err(Denied::Forbidden)
                    };
                    assert_eq!(got, want, "{role:?} {action:?} share={editors_can_share}");
                    if action == Action::Share {
                        assert_eq!(role.can_share(editors_can_share), allowed, "{role:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn no_role_or_someone_elses_trash_is_not_found() {
        for action in ACTIONS {
            assert_eq!(
                authorize(None, doc(None, false, true), action),
                Err(Denied::NotFound)
            );
            for role in [Role::Editor, Role::Commenter, Role::Viewer, Role::Link] {
                assert_eq!(
                    authorize(None, doc(Some(role), true, true), action),
                    Err(Denied::NotFound),
                    "{role:?} {action:?}"
                );
            }
        }
        assert_eq!(
            authorize(None, doc(Some(Role::Owner), true, true), Action::Restore),
            Ok(())
        );
    }

    #[test]
    fn the_owner_is_whoever_owns_it_never_a_grant() {
        let a = actor("usr_a");
        assert_eq!(
            Role::of(Some(&a), "usr_a", Some(Role::Viewer)),
            Some(Role::Owner)
        );
        assert_eq!(
            Role::of(Some(&a), "usr_b", Some(Role::Viewer)),
            Some(Role::Viewer)
        );
        assert_eq!(Role::of(Some(&a), "usr_b", Some(Role::Owner)), None);
        assert_eq!(Role::of(Some(&a), "USR_A", None), None);
        assert_eq!(Role::of(Some(&actor("")), "", None), None);
        assert_eq!(Role::of(None, "usr_a", Some(Role::Link)), Some(Role::Link));
    }

    #[test]
    fn folders_are_the_owners_only() {
        for action in ACTIONS {
            let folder = Resource::Folder { owner_id: "usr_a" };
            assert_eq!(authorize(Some(&actor("usr_a")), folder, action), Ok(()));
            assert_eq!(
                authorize(Some(&actor("usr_b")), folder, action),
                Err(Denied::NotFound)
            );
            assert_eq!(authorize(None, folder, action), Err(Denied::NotFound));
            let empty = Resource::Folder { owner_id: "" };
            assert_eq!(
                authorize(Some(&actor("")), empty, action),
                Err(Denied::NotFound)
            );
        }
    }

    #[test]
    fn download_follows_the_switch_for_non_editors() {
        assert!(Role::Owner.can_download(false));
        assert!(Role::Editor.can_download(false));
        for role in [Role::Commenter, Role::Viewer, Role::Link] {
            assert!(!role.can_download(false));
            assert!(role.can_download(true));
        }
    }
}
