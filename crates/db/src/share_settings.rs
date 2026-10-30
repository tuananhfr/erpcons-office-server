//! The share dialog's switches and the public view link of a document (`document_share_settings`).
use sqlx::{PgConnection, PgExecutor};
use time::OffsetDateTime;
use uuid::Uuid;

/// The link token only sealed (AES-256-GCM); opening it needs the server's key. No `Debug`, so
/// even the sealed bytes stay out of logs.
#[derive(Clone, sqlx::FromRow)]
pub struct SettingsRow {
    pub editors_can_share: bool,
    pub viewers_can_download: bool,
    /// The link is on and not expired.
    pub link_on: bool,
    pub link_expires_at: Option<OffsetDateTime>,
    /// Set while the link is on.
    pub link_nonce: Option<Vec<u8>>,
    pub link_sealed: Option<Vec<u8>>,
}

impl Default for SettingsRow {
    fn default() -> Self {
        Self {
            editors_can_share: true,
            viewers_can_download: true,
            link_on: false,
            link_expires_at: None,
            link_nonce: None,
            link_sealed: None,
        }
    }
}

/// The settings of `document_id`; the defaults when it has no row.
pub async fn get(db: impl PgExecutor<'_>, document_id: Uuid) -> Result<SettingsRow, sqlx::Error> {
    let row: Option<SettingsRow> = sqlx::query_as(
        "SELECT editors_can_share, viewers_can_download, link_on,
             CASE WHEN link_on THEN link_expires_at END AS link_expires_at,
             CASE WHEN link_on THEN link_token_nonce END AS link_nonce,
             CASE WHEN link_on THEN link_token_sealed END AS link_sealed
         FROM (SELECT *,
                   (link_role IS NOT NULL AND (link_expires_at IS NULL OR link_expires_at > now()))
                       AS link_on
               FROM document_share_settings WHERE document_id = $1) s",
    )
    .bind(document_id)
    .fetch_optional(db)
    .await?;
    Ok(row.unwrap_or_default())
}

pub async fn set_switches(
    conn: &mut PgConnection,
    document_id: Uuid,
    editors_can_share: bool,
    viewers_can_download: bool,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO document_share_settings (document_id, editors_can_share, viewers_can_download)
         VALUES ($1, $2, $3)
         ON CONFLICT (document_id) DO UPDATE SET
             editors_can_share = EXCLUDED.editors_can_share,
             viewers_can_download = EXCLUDED.viewers_can_download",
    )
    .bind(document_id)
    .bind(editors_can_share)
    .bind(viewers_can_download)
    .execute(conn)
    .await
    .map(drop)
}

/// A new link token: its hash for lookups and the token sealed for showing it again.
pub struct NewLink<'a> {
    pub token_hash: &'a str,
    pub nonce: &'a [u8],
    pub sealed: &'a [u8],
    pub created_by: &'a str,
    pub expires_at: Option<OffsetDateTime>,
}

/// Turns the link on with a new token, replacing any previous one.
pub async fn set_link(
    conn: &mut PgConnection,
    document_id: Uuid,
    link: &NewLink<'_>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO document_share_settings
             (document_id, link_role, link_token_hash, link_token_nonce, link_token_sealed,
              link_created_by, link_created_at, link_expires_at)
         VALUES ($1, 'viewer', $2, $3, $4, $5, now(), $6)
         ON CONFLICT (document_id) DO UPDATE SET
             link_role = 'viewer', link_token_hash = EXCLUDED.link_token_hash,
             link_token_nonce = EXCLUDED.link_token_nonce,
             link_token_sealed = EXCLUDED.link_token_sealed,
             link_created_by = EXCLUDED.link_created_by, link_created_at = now(),
             link_expires_at = EXCLUDED.link_expires_at",
    )
    .bind(document_id)
    .bind(link.token_hash)
    .bind(link.nonce)
    .bind(link.sealed)
    .bind(link.created_by)
    .bind(link.expires_at)
    .execute(conn)
    .await
    .map(drop)
}

/// Changes only the expiry of a link that is on; the token stays.
pub async fn set_link_expiry(
    conn: &mut PgConnection,
    document_id: Uuid,
    expires_at: Option<OffsetDateTime>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE document_share_settings SET link_expires_at = $2
         WHERE document_id = $1 AND link_role IS NOT NULL",
    )
    .bind(document_id)
    .bind(expires_at)
    .execute(conn)
    .await
    .map(drop)
}

/// Turns the link off; the hash and the sealed token go with it, so the token can never work again.
pub async fn clear_link(conn: &mut PgConnection, document_id: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE document_share_settings SET
             link_role = NULL, link_token_hash = NULL, link_token_nonce = NULL,
             link_token_sealed = NULL, link_created_by = NULL, link_created_at = NULL,
             link_expires_at = NULL
         WHERE document_id = $1",
    )
    .bind(document_id)
    .execute(conn)
    .await
    .map(drop)
}
