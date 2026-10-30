//! People a document is shared with, by email (plan v2 §3–4). Emails are personal data: they reach
//! the database and the mail outbox, never a log line or an audit row.
use office_db::{acl, actor_emails, locks, share_settings};
use time::OffsetDateTime;
use tracing::warn;
use uuid::Uuid;

use super::{
    Caller, Documents,
    access::{self, Accessed},
    audit::{self, Subject},
    grants::{self, Attempt, Granted, Grantor},
    invite_mail::Invite,
};
use crate::{
    Action, AddedPermissions, DocError, Email, FailPoint, LinkState, Permission, Permissions,
    Person, Role, ShareSettings,
};

const MAX_EMAILS: usize = 50;
const MAX_MESSAGE_CHARS: usize = 1000;
/// Permissions and invitations in force on one document; the share dialog lists at most these.
const MAX_PER_DOCUMENT: i64 = 500;
/// Distinct addresses one sharer may newly give access per rolling day: enough for real work, too
/// few to probe which address belongs to which account.
const MAX_NEW_RECIPIENTS: i64 = 200;
const DAY_SECS: i64 = 24 * 3600;
/// An address PI has not confirmed for this long becomes an invitation again rather than a grant:
/// it may have changed hands since.
const FRESH_EMAIL_SECS: i64 = 30 * DAY_SECS;

/// Share with people by email.
pub struct AddPermissions {
    pub emails: Vec<String>,
    /// Editor, commenter or viewer.
    pub role: Role,
    pub notify: bool,
    pub message: Option<String>,
}

/// At least one field is set.
pub struct PermissionPatch {
    pub role: Option<Role>,
    /// `Some(None)` removes the expiry.
    pub expires_at: Option<Option<OffsetDateTime>>,
}

impl Documents {
    /// The share dialog: commenters and viewers see only their own permission.
    pub async fn list_permissions(
        &self,
        c: &Caller<'_>,
        id: Uuid,
    ) -> Result<Permissions, DocError> {
        let accessed = access::load(&self.db, c, id, Action::ListPermissions).await?;
        self.permissions_of(c, &accessed).await
    }

    async fn permissions_of(
        &self,
        c: &Caller<'_>,
        accessed: &Accessed,
    ) -> Result<Permissions, DocError> {
        let id = accessed.row.id;
        let only = (!matches!(accessed.role, Role::Owner | Role::Editor)).then(|| c.actor_id());
        let rows = acl::list(&self.db, id, only, MAX_PER_DOCUMENT)
            .await
            .map_err(DocError::db)?;
        let total = acl::count(&self.db, id, only).await.map_err(DocError::db)?;
        let permissions = rows
            .into_iter()
            .map(Permission::try_from)
            .collect::<Result<Vec<_>, _>>()?;
        let settings = share_settings::get(&self.db, id)
            .await
            .map_err(DocError::db)?;
        let owner_email = actor_emails::email_of(&self.db, accessed.owner_id())
            .await
            .map_err(DocError::db)?;
        let can_share = crate::model::can_share(
            accessed.role,
            accessed.row.state,
            accessed.editors_can_share,
        );
        let token = match (&self.link_key, &settings.link_nonce, &settings.link_sealed) {
            (Some(key), Some(nonce), Some(sealed)) if can_share && settings.link_on => {
                key.open(id, nonce, sealed)
            }
            _ => None,
        };
        Ok(Permissions {
            role: accessed.role,
            owner: Person {
                actor_id: accessed.owner_id().to_owned(),
                display_name: accessed.row.owner_name.clone(),
                email: owner_email,
            },
            permissions,
            total,
            settings: ShareSettings {
                editors_can_share: settings.editors_can_share,
                viewers_can_download: settings.viewers_can_download,
            },
            link: LinkState {
                enabled: settings.link_on,
                expires_at: settings.link_expires_at,
                token,
            },
            can_share,
        })
    }

    /// Gives each address `role`: at once to an account that recently signed in with it verified,
    /// else as a pending invitation. Repeating it changes nothing and sends no second mail. Mails
    /// past a cap are skipped and counted; the access is granted all the same.
    pub async fn add_permissions(
        &self,
        c: &Caller<'_>,
        id: Uuid,
        request: AddPermissions,
    ) -> Result<AddedPermissions, DocError> {
        let result = self.add_permissions_inner(c, id, request).await;
        self.audit_failure(c, "document.share", Subject::Document(id), &result)
            .await;
        result
    }

    async fn add_permissions_inner(
        &self,
        c: &Caller<'_>,
        id: Uuid,
        request: AddPermissions,
    ) -> Result<AddedPermissions, DocError> {
        let emails = parse_emails(&request.emails)?;
        let role = grant_role(request.role)?;
        let message = request.message.as_deref().map(check_message).transpose()?;
        let share = Share {
            emails: &emails,
            role,
            notify: request.notify,
            message,
        };
        self.ensure_caller(c).await?;
        // A collision means a sign-in attached an invitation for the same person meanwhile; that
        // has committed, so the retry sees it. A second one rolled the whole request back: the
        // client is told to send it again rather than that it worked.
        let mail_skipped = match self.share_once(c, id, &share).await {
            Err(Attempt::Collision) => match self.share_once(c, id, &share).await {
                Err(Attempt::Collision) => {
                    warn!("sharing collided twice with concurrent sign-ins");
                    return Err(DocError::Contended);
                }
                other => other?,
            },
            other => other?,
        };
        let accessed = access::load(&self.db, c, id, Action::ListPermissions).await?;
        Ok(AddedPermissions {
            permissions: self.permissions_of(c, &accessed).await?,
            mail_skipped,
        })
    }

    /// One attempt at sharing, in one transaction; the number of mails skipped by the caps.
    async fn share_once(
        &self,
        c: &Caller<'_>,
        id: Uuid,
        share: &Share<'_>,
    ) -> Result<u32, Attempt> {
        let Share {
            emails,
            role,
            notify,
            message,
        } = *share;
        let mut tx = self.begin().await?;
        // Locking the document row serializes sharing changes of one document.
        let accessed = access::load_for_update(&mut tx, c, id, Action::Share).await?;
        let owner_id = accessed.owner_id().to_owned();
        let owner = matches!(accessed.role, Role::Owner);
        let owner_email = actor_emails::email_of(&mut *tx, &owner_id)
            .await
            .map_err(DocError::db)?;
        let own_email = if owner {
            owner_email.clone()
        } else {
            actor_emails::email_of(&mut *tx, c.actor_id())
                .await
                .map_err(DocError::db)?
        };
        let grantor = Grantor {
            actor_id: c.actor_id(),
            document_id: id,
            owner,
            own_expiry: accessed.grant_expires_at,
            role,
        };
        let mut granted = Vec::new();
        let mut new_recipient = false;
        for email in emails {
            let address = Some(email.as_str());
            if owner_email.as_deref() == address || own_email.as_deref() == address {
                continue;
            }
            let outcome = match actor_emails::actor_of(&mut *tx, email.as_str(), FRESH_EMAIL_SECS)
                .await
                .map_err(DocError::db)?
            {
                Some(actor) if actor == owner_id || actor == c.actor_id() => Granted::Unchanged,
                Some(actor) => grants::grant_actor(&mut tx, grantor, &actor, email).await?,
                None => grants::grant_pending(&mut tx, grantor, email).await?,
            };
            new_recipient |= outcome == Granted::New;
            if outcome != Granted::Unchanged {
                granted.push(email);
            }
        }
        self.fail_points.pause(FailPoint::ShareCollision).await;
        if self.fail_points.fires(FailPoint::ShareCollision) {
            return Err(Attempt::Collision);
        }
        if !granted.is_empty()
            && acl::count(&mut *tx, id, None).await.map_err(DocError::db)? > MAX_PER_DOCUMENT
        {
            return Err(DocError::TooManyPermissions.into());
        }
        if new_recipient {
            locks::recipients(&mut tx, c.actor_id())
                .await
                .map_err(DocError::db)?;
            let recipients = acl::recipients_since(&mut *tx, c.actor_id(), DAY_SECS)
                .await
                .map_err(DocError::db)?;
            if recipients > MAX_NEW_RECIPIENTS {
                return Err(DocError::TooManyRecipients.into());
            }
        }
        let mail_skipped = if notify && self.mail.enabled && !granted.is_empty() {
            let invite = Invite { role, message };
            self.queue_invites(&mut tx, c, &accessed, &invite, &granted)
                .await?
        } else {
            0
        };
        audit::success(&mut tx, c, "document.share", Subject::Document(id)).await?;
        tx.commit().await.map_err(DocError::db)?;
        Ok(mail_skipped)
    }

    pub async fn update_permission(
        &self,
        c: &Caller<'_>,
        id: Uuid,
        permission_id: Uuid,
        patch: PermissionPatch,
    ) -> Result<Permission, DocError> {
        let result = self
            .update_permission_inner(c, id, permission_id, patch)
            .await;
        self.audit_failure(
            c,
            "document.permission_update",
            Subject::Document(id),
            &result,
        )
        .await;
        result
    }

    async fn update_permission_inner(
        &self,
        c: &Caller<'_>,
        id: Uuid,
        permission_id: Uuid,
        patch: PermissionPatch,
    ) -> Result<Permission, DocError> {
        if patch.role.is_none() && patch.expires_at.is_none() {
            return Err(DocError::InvalidRequest);
        }
        let role = patch.role.map(grant_role).transpose()?;
        if let Some(Some(at)) = patch.expires_at {
            future(at)?;
        }
        let mut tx = self.begin().await?;
        let accessed = access::load_for_update(&mut tx, c, id, Action::Share).await?;
        let target = acl::get(&mut *tx, id, permission_id)
            .await
            .map_err(DocError::db)?
            .ok_or(DocError::NotFound)?;
        // An editor who may share could otherwise lift its own expiry or raise its own role.
        if target.actor_id.as_deref() == Some(c.actor_id()) {
            return Err(DocError::Forbidden);
        }
        let owner = matches!(accessed.role, Role::Owner);
        if patch.expires_at.is_some() && !owner {
            return Err(DocError::Forbidden);
        }
        let expires_at = match role {
            Some(role) if !owner => {
                let end = grants::after_role_change(
                    target.expires_at,
                    &target.role,
                    role.as_str(),
                    accessed.grant_expires_at,
                );
                (end != target.expires_at).then_some(end)
            }
            _ => patch.expires_at,
        };
        let change = acl::PermissionChange {
            role: role.map(Role::as_str),
            expires_at,
        };
        if !acl::update(&mut tx, id, permission_id, &change)
            .await
            .map_err(DocError::db)?
        {
            return Err(DocError::NotFound);
        }
        audit::success(
            &mut tx,
            c,
            "document.permission_update",
            Subject::Document(id),
        )
        .await?;
        let row = acl::get(&mut *tx, id, permission_id)
            .await
            .map_err(DocError::db)?
            .ok_or(DocError::NotFound)?;
        tx.commit().await.map_err(DocError::db)?;
        Permission::try_from(row)
    }

    /// Removes a permission or a pending invitation; anyone may remove their own.
    pub async fn remove_permission(
        &self,
        c: &Caller<'_>,
        id: Uuid,
        permission_id: Uuid,
    ) -> Result<(), DocError> {
        let result = self.remove_permission_inner(c, id, permission_id).await;
        self.audit_failure(c, "document.unshare", Subject::Document(id), &result)
            .await;
        result
    }

    async fn remove_permission_inner(
        &self,
        c: &Caller<'_>,
        id: Uuid,
        permission_id: Uuid,
    ) -> Result<(), DocError> {
        let mut tx = self.begin().await?;
        let accessed = access::load_for_update(&mut tx, c, id, Action::Read).await?;
        let row = acl::get(&mut *tx, id, permission_id)
            .await
            .map_err(DocError::db)?;
        let own = row
            .as_ref()
            .is_some_and(|r| r.actor_id.as_deref() == Some(c.actor_id()));
        // Checked before the existence of someone else's permission, which only sharers may learn.
        accessed.allows(c.actor, if own { Action::Leave } else { Action::Share })?;
        // Removing then inviting again would lift an expiry, which only the owner may do.
        let time_limited = row.as_ref().is_some_and(|r| r.expires_at.is_some());
        if !own && time_limited && !matches!(accessed.role, Role::Owner) {
            return Err(DocError::Forbidden);
        }
        if row.is_none()
            || !acl::revoke(&mut tx, id, permission_id)
                .await
                .map_err(DocError::db)?
        {
            return Err(DocError::NotFound);
        }
        audit::success(&mut tx, c, "document.unshare", Subject::Document(id)).await?;
        tx.commit().await.map_err(DocError::db)
    }
}

/// What to share, kept across a retry.
#[derive(Clone, Copy)]
struct Share<'a> {
    emails: &'a [Email],
    role: Role,
    notify: bool,
    message: Option<&'a str>,
}

fn parse_emails(raw: &[String]) -> Result<Vec<Email>, DocError> {
    if raw.is_empty() || raw.len() > MAX_EMAILS {
        return Err(DocError::InvalidRequest);
    }
    let mut emails: Vec<Email> = Vec::with_capacity(raw.len());
    for r in raw {
        let email = Email::parse(r).map_err(|_| DocError::InvalidRequest)?;
        if !emails.iter().any(|e| e.as_str() == email.as_str()) {
            emails.push(email);
        }
    }
    Ok(emails)
}

fn grant_role(role: Role) -> Result<Role, DocError> {
    match role {
        Role::Editor | Role::Commenter | Role::Viewer => Ok(role),
        Role::Owner | Role::Link => Err(DocError::InvalidRequest),
    }
}

/// Goes into a mail as plain text: line breaks and tabs only.
fn check_message(message: &str) -> Result<&str, DocError> {
    let ok = message.chars().count() <= MAX_MESSAGE_CHARS
        && !message
            .chars()
            .any(|ch| ch.is_control() && !matches!(ch, '\n' | '\r' | '\t'));
    if ok {
        Ok(message)
    } else {
        Err(DocError::InvalidRequest)
    }
}

pub(super) fn future(at: OffsetDateTime) -> Result<(), DocError> {
    if at > OffsetDateTime::now_utc() {
        Ok(())
    } else {
        Err(DocError::InvalidRequest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emails_are_deduplicated_after_normalizing() {
        let raw = vec![
            "A@x.vn".to_owned(),
            " a@X.vn ".to_owned(),
            "b@x.vn".to_owned(),
        ];
        let emails = parse_emails(&raw).expect("valid");
        assert_eq!(emails.len(), 2);
        assert!(parse_emails(&[]).is_err());
        assert!(parse_emails(&vec!["a@x.vn".to_owned(); MAX_EMAILS + 1]).is_err());
        assert!(parse_emails(&["nope".to_owned()]).is_err());
    }

    #[test]
    fn messages_are_plain_text() {
        assert!(check_message("Chào anh,\nxem giúp em.\t").is_ok());
        assert!(check_message("a\u{1b}[31m").is_err());
        assert!(check_message(&"x".repeat(MAX_MESSAGE_CHARS + 1)).is_err());
    }

    #[test]
    fn only_grantable_roles() {
        assert!(grant_role(Role::Viewer).is_ok());
        assert!(grant_role(Role::Owner).is_err());
        assert!(grant_role(Role::Link).is_err());
    }
}
