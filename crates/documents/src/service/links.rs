//! The public view link. The token is looked up by its sha256 and otherwise kept only sealed under
//! the server's key, so sharers can copy it again; it is never logged or audited.
use office_db::share_settings::{self, NewLink};
use time::OffsetDateTime;
use tracing::warn;
use uuid::Uuid;

use super::{
    Caller, Content, Documents,
    access::{self, Accessed},
    audit::{self, Subject},
    grants::earliest,
    permissions::future,
};
use crate::{Action, DocError, Document, LinkCreated, Role, link_token};

impl Documents {
    /// Turns the link on. While it is already on, keeps its token, so a repeated call is harmless; a
    /// new token takes turning the link off first. `expires_at` is `None` when the request names
    /// none. Expiries are the owner's: another sharer's link ends no later than its own access,
    /// and it cannot move the end of a link that is on.
    pub async fn enable_link(
        &self,
        c: &Caller<'_>,
        id: Uuid,
        expires_at: Option<Option<OffsetDateTime>>,
    ) -> Result<LinkCreated, DocError> {
        let result = self.enable_link_inner(c, id, expires_at).await;
        self.audit_failure(c, "document.link_enable", Subject::Document(id), &result)
            .await;
        result
    }

    async fn enable_link_inner(
        &self,
        c: &Caller<'_>,
        id: Uuid,
        expires_at: Option<Option<OffsetDateTime>>,
    ) -> Result<LinkCreated, DocError> {
        let requested = expires_at.flatten();
        if let Some(at) = requested {
            future(at)?;
        }
        let Some(key) = self.link_key.as_ref() else {
            warn!("no link key configured: links cannot be turned on");
            return Err(DocError::Unavailable);
        };
        self.ensure_caller(c).await?;
        let mut tx = self.begin().await?;
        let accessed = access::load_for_update(&mut tx, c, id, Action::Share).await?;
        let owner = matches!(accessed.role, Role::Owner);
        let current = share_settings::get(&mut *tx, id)
            .await
            .map_err(DocError::db)?;
        let current_end = current.link_expires_at;
        let kept = match (current.link_on, current.link_nonce, current.link_sealed) {
            (true, Some(nonce), Some(sealed)) => {
                let opened = key.open(id, &nonce, &sealed);
                if opened.is_none() {
                    // Sealed under an earlier key: the old link keeps working until replaced here.
                    warn!("a link token did not open with the current link key; issuing a new one");
                }
                opened
            }
            _ => None,
        };
        let (token, expires_at) = match kept {
            Some(token) if owner => {
                // Absent or null: the owner's link has no end.
                share_settings::set_link_expiry(&mut tx, id, requested)
                    .await
                    .map_err(DocError::db)?;
                (token, requested)
            }
            Some(token) => {
                if expires_at.is_some_and(|asked| asked != current_end) {
                    return Err(DocError::Forbidden);
                }
                (token, current_end)
            }
            None => {
                let expires_at = if owner {
                    requested
                } else {
                    let end = earliest(requested, accessed.grant_expires_at);
                    if current.link_on {
                        earliest(end, current_end)
                    } else {
                        end
                    }
                };
                let token = link_token::generate()?;
                let hash = link_token::hash(&token).ok_or(DocError::Unavailable)?;
                let sealed = key.seal(id, &token)?;
                let link = NewLink {
                    token_hash: &hash,
                    nonce: &sealed.nonce,
                    sealed: &sealed.sealed,
                    created_by: c.actor_id(),
                    expires_at,
                };
                share_settings::set_link(&mut tx, id, &link)
                    .await
                    .map_err(DocError::db)?;
                (token, expires_at)
            }
        };
        audit::success(&mut tx, c, "document.link_enable", Subject::Document(id)).await?;
        tx.commit().await.map_err(DocError::db)?;
        Ok(LinkCreated { token, expires_at })
    }

    /// Turns the link off; a no-op when it is already off.
    pub async fn disable_link(&self, c: &Caller<'_>, id: Uuid) -> Result<(), DocError> {
        let result = self.disable_link_inner(c, id).await;
        self.audit_failure(c, "document.link_disable", Subject::Document(id), &result)
            .await;
        result
    }

    async fn disable_link_inner(&self, c: &Caller<'_>, id: Uuid) -> Result<(), DocError> {
        let mut tx = self.begin().await?;
        let accessed = access::load_for_update(&mut tx, c, id, Action::Share).await?;
        // Off then on again would lift the owner's end of the link.
        if !matches!(accessed.role, Role::Owner) {
            let current = share_settings::get(&mut *tx, id)
                .await
                .map_err(DocError::db)?;
            if current.link_on && current.link_expires_at.is_some() {
                return Err(DocError::Forbidden);
            }
        }
        share_settings::clear_link(&mut tx, id)
            .await
            .map_err(DocError::db)?;
        audit::success(&mut tx, c, "document.link_disable", Subject::Document(id)).await?;
        tx.commit().await.map_err(DocError::db)
    }

    /// The document behind a link token, for anyone holding it.
    pub async fn open_link(&self, token: &str, trace_id: &str) -> Result<Document, DocError> {
        let accessed = self.by_link(token).await?;
        audit::link(&self.db, "link.open", accessed.row.id, trace_id).await;
        accessed.document()
    }

    pub async fn open_link_content(
        &self,
        token: &str,
        trace_id: &str,
    ) -> Result<Content, DocError> {
        let accessed = self.by_link(token).await?;
        let content = self.current_content(accessed.row.id).await?;
        audit::link(&self.db, "link.content", accessed.row.id, trace_id).await;
        Ok(content)
    }

    async fn by_link(&self, token: &str) -> Result<Accessed, DocError> {
        // A malformed token is refused before it reaches the database.
        let hash = link_token::hash(token).ok_or(DocError::NotFound)?;
        let row = office_db::access::of_link(&self.db, &hash)
            .await
            .map_err(DocError::db)?;
        access::decide(row, None, true, Action::Read)
    }
}
