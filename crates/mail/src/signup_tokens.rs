use async_trait::async_trait;
use uuid::Uuid;

use crate::SendError;

/// A verification token just issued for a pending sign-up. No `Debug`: a secret and a name.
pub struct IssuedToken {
    pub display_name: String,
    pub token: String,
}

/// Issues the token of a verification mail at the moment it is sent, so a token exists only in its
/// hash and in the mail. Each attempt voids the previous token.
#[async_trait]
pub trait SignupTokens: Send + Sync {
    /// `Ok(None)`: the sign-up is gone or no longer pending, and its mail is dropped.
    async fn issue(&self, signup_id: Uuid) -> Result<Option<IssuedToken>, SendError>;
}
