//! Who is calling: resolves a PI ID access token to an Office actor (plan §2.1).
mod actor;
mod cache;
mod me_endpoint;
mod single_flight;
#[cfg(feature = "testing")]
pub mod testing;
mod verifier;

pub use actor::Actor;
pub use me_endpoint::{MeEndpointConfig, MeEndpointVerifier};
pub use verifier::{IdentityVerifier, VerifyError};
