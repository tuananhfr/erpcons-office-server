//! Postgres access: pool, embedded migrations and the queries the API runs.
pub mod actors;
pub mod audit;
pub mod documents;
mod errors;
pub mod folders;
pub mod idempotency;
pub mod locks;
pub mod objects;
mod pool;
pub mod purge;
pub mod quota;
mod state;
#[cfg(feature = "testing")]
pub mod testing;
pub mod versions;

pub use errors::is_unique_violation;
pub use pool::{PoolConfig, connect, connect_lazy, migrate, ping};
pub use sqlx::{
    Error, PgConnection, PgExecutor, PgPool, Postgres, Transaction, migrate::MigrateError,
};
pub use state::{ItemState, UnknownState};
