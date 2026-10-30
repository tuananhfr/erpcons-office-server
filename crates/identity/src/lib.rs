//! Who is calling: resolves a PI ID access token to an Office actor (plan §2.1).
mod actor;
mod cache;
mod http;
mod jwks;
mod jwt;
mod jwt_verifier;
mod profile;
mod single_flight;
#[cfg(feature = "testing")]
pub mod testing;
mod verifier;

pub use actor::{Actor, EmailClaim};
pub use jwt_verifier::{JwtVerifier, JwtVerifierConfig};
pub use verifier::{IdentityVerifier, VerifyError};
