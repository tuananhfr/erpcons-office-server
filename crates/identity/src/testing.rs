//! Test helpers: an in-process PI stand-in that signs access tokens like PI ID does (EdDSA JWT,
//! `kid`) and serves `/id/jwks.json` and `/id/userinfo`.
mod fake_pi;
mod signer;

pub use fake_pi::{DEFAULT_NAME, DEFAULT_SUBJECT, FakePi, OFFICE_CLIENT};
pub use signer::{SigningKey, segment};
