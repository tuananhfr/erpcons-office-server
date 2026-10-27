//! Postgres access: pool, embedded migrations and the queries the API runs.
pub mod actors;
pub mod audit;
mod pool;
pub mod quota;
#[cfg(feature = "testing")]
pub mod testing;

pub use pool::{PoolConfig, connect, connect_lazy, migrate, ping};
pub use sqlx::{Error, PgPool, migrate::MigrateError};
