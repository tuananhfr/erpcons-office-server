//! Self sign-up (office-licensing plan, part D). A sign-up is checked and stored with a bcrypt hash of
//! its password, the address is verified through a one-time link sent by the mail outbox, and only
//! then does a background job ask PI to create (or reuse) the PI ID account
//! (`POST /api/v1/service/accounts/register`, D515). The hash is dropped as soon as PI has answered
//! for good; unverified sign-ups are deleted after 7 days. Office keeps no account of its own.
mod config;
mod error;
mod expiry;
mod input;
mod password;
mod pi;
mod push;
mod service;
mod token;

pub use config::{
    DEFAULT_DAILY_CAP, PER_ADDRESS_DAILY_CAP, SignupPolicy, TOKEN_TTL, UNVERIFIED_RETENTION,
};
pub use error::{Field, SignupError};
pub use expiry::{ExpiryReport, expire};
pub use input::SignupRequest;
pub use password::{BCRYPT_COST, PasswordHashError, hash_password, portable_hash};
pub use pi::{
    ClientError, HttpPiRegistrar, PiRegistrar, PiRegistrarConfig, RegisterError, RegisterRequest,
    Registered, ServiceKey,
};
pub use push::{PushConfig, PushReport, run_push};
pub use service::Signups;
