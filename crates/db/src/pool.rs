use std::{str::FromStr, time::Duration};

use sqlx::{
    PgPool,
    migrate::MigrateError,
    postgres::{PgConnectOptions, PgPoolOptions},
};

/// Pool settings. Deliberately without `Debug`: the URL carries the password.
#[derive(Clone)]
pub struct PoolConfig {
    pub url: String,
    pub max_connections: u32,
    pub acquire_timeout: Duration,
}

impl PoolConfig {
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            // The production host is shared with other sites (plan §8).
            max_connections: 10,
            acquire_timeout: Duration::from_secs(5),
        }
    }

    fn options(&self) -> Result<(PgPoolOptions, PgConnectOptions), sqlx::Error> {
        let connect = PgConnectOptions::from_str(&self.url)?;
        let pool = PgPoolOptions::new()
            .max_connections(self.max_connections)
            .acquire_timeout(self.acquire_timeout);
        Ok((pool, connect))
    }
}

/// Connects now, failing when the database is unreachable.
pub async fn connect(config: &PoolConfig) -> Result<PgPool, sqlx::Error> {
    let (pool, connect) = config.options()?;
    pool.connect_with(connect).await
}

/// Connects on first use.
pub fn connect_lazy(config: &PoolConfig) -> Result<PgPool, sqlx::Error> {
    let (pool, connect) = config.options()?;
    Ok(pool.connect_lazy_with(connect))
}

/// Applies the embedded migrations that are not applied yet.
pub async fn migrate(pool: &PgPool) -> Result<(), MigrateError> {
    let mut migrator = sqlx::migrate!("./migrations");
    // A rollback starts the previous release on a schema that already has the newer, add-only
    // migrations (plan §8); refusing them would make the rollback fail to start.
    migrator.set_ignore_missing(true);
    migrator.run(pool).await
}

pub async fn ping(pool: &PgPool) -> Result<(), sqlx::Error> {
    sqlx::query("SELECT 1").execute(pool).await.map(drop)
}
