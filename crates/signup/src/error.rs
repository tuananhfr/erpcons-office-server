/// The input field a refused sign-up names; the site checks the same rules first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field {
    Name,
    Email,
    Password,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SignupError {
    /// Sign-up is not configured on this server (no SMTP or no PI service key).
    #[error("sign-up is closed")]
    Closed,
    #[error("invalid sign-up field {0:?}")]
    Invalid(Field),
    /// The server-wide daily cap is reached.
    #[error("daily sign-up cap reached")]
    DailyLimit,
    /// Unknown, used, replaced or expired verification token.
    #[error("invalid verification token")]
    InvalidToken,
    /// Database or random source failure; the cause is logged where it happens.
    #[error("sign-up unavailable")]
    Unavailable,
}

impl From<office_db::Error> for SignupError {
    fn from(err: office_db::Error) -> Self {
        tracing::error!(error = %err, "sign-up database error");
        Self::Unavailable
    }
}
