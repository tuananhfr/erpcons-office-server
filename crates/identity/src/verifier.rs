use async_trait::async_trait;

use crate::Actor;

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum VerifyError {
    /// Missing, malformed, expired, revoked or otherwise refused token.
    #[error("token rejected")]
    Unauthenticated,
    /// The identity provider could not answer; retrying later may succeed.
    #[error("identity provider unavailable")]
    Unavailable,
}

/// Resolves an access token to an actor. Implementations never log the token.
#[async_trait]
pub trait IdentityVerifier: Send + Sync {
    async fn verify(&self, token: &str) -> Result<Actor, VerifyError>;
}
