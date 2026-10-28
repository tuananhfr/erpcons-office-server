//! Who is calling: resolves a PI ID access token to an Office actor (plan §2.1).
mod actor;
mod cache;
mod display_name;
mod http;
mod jwks;
mod jwt;
mod jwt_verifier;
mod single_flight;
#[cfg(feature = "testing")]
pub mod testing;
mod verifier;

pub use actor::Actor;
pub use jwt_verifier::{JwtVerifier, JwtVerifierConfig};
pub use verifier::{IdentityVerifier, VerifyError};
