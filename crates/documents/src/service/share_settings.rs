use office_db::share_settings;
use uuid::Uuid;

use super::{
    Caller, Documents, access,
    audit::{self, Subject},
};
use crate::{Action, DocError, ShareSettings};

impl Documents {
    /// The two switches of the share dialog; the owner's alone.
    pub async fn put_share_settings(
        &self,
        c: &Caller<'_>,
        id: Uuid,
        settings: ShareSettings,
    ) -> Result<ShareSettings, DocError> {
        let result = self.put_share_settings_inner(c, id, settings).await;
        self.audit_failure(c, "document.share_settings", Subject::Document(id), &result)
            .await;
        result
    }

    async fn put_share_settings_inner(
        &self,
        c: &Caller<'_>,
        id: Uuid,
        settings: ShareSettings,
    ) -> Result<ShareSettings, DocError> {
        let mut tx = self.begin().await?;
        access::load_for_update(&mut tx, c, id, Action::ChangeShareSettings).await?;
        share_settings::set_switches(
            &mut tx,
            id,
            settings.editors_can_share,
            settings.viewers_can_download,
        )
        .await
        .map_err(DocError::db)?;
        audit::success(&mut tx, c, "document.share_settings", Subject::Document(id)).await?;
        tx.commit().await.map_err(DocError::db)?;
        Ok(settings)
    }
}
