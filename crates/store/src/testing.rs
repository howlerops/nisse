//! Scratch databases for integration tests. Compiled only with the `test-support` feature.
//!
//! A test calls [`ScratchDb::create`], which makes an empty database on the server named by
//! `TEST_DATABASE_URL`, applies every migration with the real `scripts/migrate.sh`, and hands
//! back a connected client. Tests that need a database fail loudly when the variable is not
//! set: a store test that silently skips proves nothing.

use std::{
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicU32, Ordering},
};

use tokio_postgres::Client;

use crate::{StoreError, connect};

/// Environment variable naming the Postgres server to create scratch databases on, e.g.
/// `postgres://postgres@localhost:5432/postgres`. The role needs `CREATEDB`.
pub const ENV_TEST_DATABASE_URL: &str = "TEST_DATABASE_URL";

static COUNTER: AtomicU32 = AtomicU32::new(0);

/// A freshly migrated database that exists for one test.
pub struct ScratchDb {
    /// Connection string of the scratch database.
    pub url: String,
    admin_url: String,
    name: String,
}

impl ScratchDb {
    /// Creates and migrates a scratch database.
    ///
    /// # Panics
    ///
    /// Panics when `TEST_DATABASE_URL` is unset or the database cannot be created or
    /// migrated. This is test support: failing loudly is the point.
    #[allow(
        clippy::expect_used,
        clippy::panic,
        reason = "test support fails loudly"
    )]
    pub async fn create() -> Self {
        let admin_url = std::env::var(ENV_TEST_DATABASE_URL).unwrap_or_else(|_| {
            panic!(
                "{ENV_TEST_DATABASE_URL} is not set. Store tests need a Postgres server; run \
                 scripts/dev-postgres.sh and export the URL it prints."
            )
        });
        let name = format!(
            "nisse_rs_{}_{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        );
        let admin = connect(&admin_url).await.expect("connect to test server");
        admin
            .batch_execute(&format!("CREATE DATABASE {name}"))
            .await
            .expect("create scratch database");

        let (base, query) = match admin_url.split_once('?') {
            Some((base, query)) => (base, format!("?{query}")),
            None => (admin_url.as_str(), String::new()),
        };
        let server = base.rsplit_once('/').map_or(base, |(server, _db)| server);
        let url = format!("{server}/{name}{query}");

        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let status = Command::new(root.join("scripts/migrate.sh"))
            .env("DATABASE_URL", &url)
            .output()
            .expect("run scripts/migrate.sh (needs psql)");
        assert!(
            status.status.success(),
            "migrations failed: {}",
            String::from_utf8_lossy(&status.stderr)
        );
        Self {
            url,
            admin_url,
            name,
        }
    }

    /// A new connection to the scratch database.
    pub async fn client(&self) -> Result<Client, StoreError> {
        connect(&self.url).await
    }

    /// Drops the scratch database. Call it at the end of the test; a test that panics first
    /// leaves its database behind, which only matters on a long-lived development server.
    pub async fn drop_db(self) -> Result<(), StoreError> {
        let admin = connect(&self.admin_url).await?;
        admin
            .batch_execute(&format!(
                "DROP DATABASE IF EXISTS {} WITH (FORCE)",
                self.name
            ))
            .await?;
        Ok(())
    }
}
