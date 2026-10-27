//! Fresh databases for tests on the server named by `TEST_DATABASE_URL`, dropped afterwards, so
//! tests run in parallel without sharing rows.
use std::{str::FromStr, thread};

use sqlx::{
    Connection, Executor, PgConnection, PgPool,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use uuid::Uuid;

pub const ENV: &str = "TEST_DATABASE_URL";

pub struct TestDb {
    pool: PgPool,
    admin: PgConnectOptions,
    name: String,
}

impl TestDb {
    /// A new migrated database, or `None` (noted on stderr) when `TEST_DATABASE_URL` is unset.
    pub async fn create() -> Option<Self> {
        let Ok(url) = std::env::var(ENV) else {
            eprintln!("skipped: {ENV} is not set");
            return None;
        };
        let admin = PgConnectOptions::from_str(&url).expect("TEST_DATABASE_URL is a Postgres URL");
        let name = format!("office_test_{}", Uuid::new_v4().simple());
        let mut conn = PgConnection::connect_with(&admin)
            .await
            .expect("connect to TEST_DATABASE_URL");
        conn.execute(format!("CREATE DATABASE {name}").as_str())
            .await
            .expect("create the test database");
        conn.close().await.ok();
        let pool = PgPoolOptions::new()
            .max_connections(5)
            .connect_with(admin.clone().database(&name))
            .await
            .expect("connect to the test database");
        crate::migrate(&pool)
            .await
            .expect("migrate the test database");
        Some(Self { pool, admin, name })
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }
}

impl Drop for TestDb {
    fn drop(&mut self) {
        let (admin, name) = (self.admin.clone(), self.name.clone());
        // Drop runs on the test's runtime, which must not be blocked on; use a runtime of its own.
        let _ = thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("runtime");
            runtime.block_on(async move {
                if let Ok(mut conn) = PgConnection::connect_with(&admin).await {
                    let drop = format!("DROP DATABASE IF EXISTS {name} WITH (FORCE)");
                    let _ = conn.execute(drop.as_str()).await;
                }
            });
        })
        .join();
    }
}
