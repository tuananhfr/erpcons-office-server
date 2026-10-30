//! Keeps the actor row and its verified email in step with PI, and hands the actor the invitations
//! waiting for that email.
use office_db::{acl, actor_emails, actors};
use office_identity::{Actor, EmailClaim};
use tracing::warn;

use super::Documents;
use crate::{DocError, Email};

impl Documents {
    /// Called when a token is verified (not on every request: the verification is cached). Only an
    /// email PI reports as verified is recorded: an unverified one could claim someone else's
    /// invitations. Permissions already attached through an earlier email stay.
    pub async fn sync_actor(&self, actor: &Actor) -> Result<(), DocError> {
        match self.sync_once(actor).await {
            // A share inserted a permission for this person meanwhile; the retry sees it.
            Err(Sync::Collision) => match self.sync_once(actor).await {
                Err(Sync::Collision) => {
                    // Best effort: the next verification attaches what is still pending.
                    warn!("attaching invitations collided twice with concurrent sharing");
                    Ok(())
                }
                Err(Sync::Failed(err)) => Err(err),
                Ok(()) => Ok(()),
            },
            Err(Sync::Failed(err)) => Err(err),
            Ok(()) => Ok(()),
        }
    }

    async fn sync_once(&self, actor: &Actor) -> Result<(), Sync> {
        let mut tx = self.begin().await?;
        actors::upsert_seen(&mut *tx, &actor.actor_id, &actor.display_name)
            .await
            .map_err(DocError::db)?;
        let verified = match &actor.email {
            // PI said nothing about the email (e.g. a token without the claim): keep what we have.
            EmailClaim::Unknown => None,
            EmailClaim::Unverified => Some(None),
            EmailClaim::Verified(raw) => match Email::parse(raw) {
                Ok(email) => Some(Some(email)),
                Err(_) => {
                    warn!("PI reported a verified email the server cannot use");
                    Some(None)
                }
            },
        };
        match verified {
            None => {}
            Some(None) => {
                actor_emails::forget(&mut tx, &actor.actor_id)
                    .await
                    .map_err(DocError::db)?;
            }
            Some(Some(email)) => {
                actor_emails::upsert_verified(&mut tx, &actor.actor_id, email.as_str())
                    .await
                    .map_err(DocError::db)?;
                acl::attach_pending(&mut tx, &actor.actor_id, email.as_str())
                    .await
                    .map_err(Sync::from_attach)?;
            }
        }
        tx.commit().await.map_err(DocError::db)?;
        Ok(())
    }
}

/// Why one sync attempt stopped: a race worth one retry, or a real failure.
enum Sync {
    Collision,
    Failed(DocError),
}

impl Sync {
    fn from_attach(err: office_db::Error) -> Self {
        if acl::is_collision(&err) {
            Self::Collision
        } else {
            Self::Failed(DocError::db(err))
        }
    }
}

impl From<DocError> for Sync {
    fn from(err: DocError) -> Self {
        Self::Failed(err)
    }
}
