use std::time::Duration;

/// Sign-ups the whole server accepts per rolling 24 hours (verification mails queued), unless
/// configured otherwise.
pub const DEFAULT_DAILY_CAP: u32 = 100;
/// Verification mails one address receives per rolling 24 hours; past it a sign-up is answered as
/// usual but sends nothing, so the answer does not tell whether the address was used.
pub const PER_ADDRESS_DAILY_CAP: i64 = 3;
/// A verification link works this long after its mail was sent.
pub const TOKEN_TTL: Duration = Duration::from_secs(office_mail::VERIFY_TTL_HOURS as u64 * 3600);
/// Sign-ups never verified are deleted this long after their last submission.
pub const UNVERIFIED_RETENTION: Duration = Duration::from_secs(7 * 24 * 3600);

/// Whether sign-up is open, and its cap.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SignupPolicy {
    /// Open only with SMTP (the link must reach the address) and PI's service key (the account must
    /// reach PI); closed by default.
    pub enabled: bool,
    pub daily_cap: u32,
}

impl Default for SignupPolicy {
    fn default() -> Self {
        Self {
            enabled: false,
            daily_cap: DEFAULT_DAILY_CAP,
        }
    }
}
