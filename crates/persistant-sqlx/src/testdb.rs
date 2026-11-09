// SPDX-License-Identifier: MIT OR Apache-2.0
//! A fresh PostgreSQL database for one test, in place of `#[sqlx::test]` (ROADMAP row 29, Humboldt).
//!
//! ```ignore
//! let admin = PgBackend::connect(&std::env::var("DATABASE_URL")?).await?;
//! let db = TestDb::create(&admin).await?;         // CREATE DATABASE persistant_test_<unique>
//! db.migrate(&migrations).await?;                  // optional: the schema the test needs
//! let pool = db.backend().pool();                  // the sqlx pool, for `sqlx::query!` and the rest
//! // ... the test ...
//! db.drop_database().await?;                       // or just let it go out of scope
//! ```
//!
//! Every [`TestDb`] is its own database, so tests that run at the same time cannot see each other's
//! rows, and nothing needs `--test-threads=1`. The admin connection needs `CREATEDB`.
//!
//! **Cleanup.** [`TestDb::drop_database`] closes the pool and drops the database (`WITH (FORCE)`, so a
//! connection a test leaked does not block it) and reports an error. A `TestDb` dropped without it, a
//! test that panicked, say, is dropped the same way from a helper thread with its own runtime, so the
//! database does not outlive the test binary; a failure there cannot be reported and is ignored (a
//! later run's databases have different names, so a leftover costs disk, never correctness).
//!
//! PostgreSQL only: it is the engine Humboldt tests on. SQLite tests use a temporary file and MySQL a
//! scratch database ([`crate::MysqlBackend::verify_fresh`]).

use crate::PgBackend;
use persistant_relational::{MigrateError, MigrateReport, Migration, RelError};
use sqlx::ConnectOptions;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A database created for one test and dropped when the test is done.
#[derive(Debug)]
pub struct TestDb {
    admin: PgConnectOptions,
    name: String,
    // `None` once dropped.
    backend: Option<PgBackend>,
}

fn unique_name() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    format!(
        "persistant_test_{}_{}_{}",
        std::process::id(),
        nanos,
        COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

fn failed(what: &str, e: sqlx::Error) -> RelError {
    RelError::Backend(format!("{what}: {e}"))
}

impl TestDb {
    /// Create an empty database on the server `admin` is connected to and connect to it (a pool of up
    /// to 8 connections, the admin connection's options otherwise).
    pub async fn create(admin: &PgBackend) -> Result<TestDb, RelError> {
        let name = unique_name();
        // The name is made here from digits and a fixed prefix, so it needs no quoting.
        sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {name}")))
            .execute(admin.pool())
            .await
            .map_err(|e| failed("create the test database (does the role have CREATEDB?)", e))?;
        let admin_options = (*admin.pool().connect_options()).clone();
        let options = admin_options
            .clone()
            .database(&name)
            .disable_statement_logging();
        let connected = PgPoolOptions::new()
            .max_connections(8)
            .connect_with(options)
            .await;
        let backend = match connected {
            Ok(pool) => PgBackend::from_pool(pool).await,
            Err(e) => Err(failed("connect to the test database", e)),
        };
        match backend {
            Ok(backend) => Ok(TestDb {
                admin: admin_options.clone(),
                name,
                backend: Some(backend),
            }),
            Err(e) => {
                let _ = drop_named(&admin_options, &name).await;
                Err(e)
            }
        }
    }

    /// The database's name (`persistant_test_...`).
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The backend on the test database. Its [`pool`](PgBackend::pool) is the sqlx pool for
    /// `sqlx::query!` and anything this layer does not carry.
    pub fn backend(&self) -> &PgBackend {
        self.backend
            .as_ref()
            .expect("the test database is dropped only by consuming it")
    }

    /// Apply `migrations` to the test database (see [`PgBackend::migrate`]).
    pub async fn migrate(&self, migrations: &[Migration]) -> Result<MigrateReport, MigrateError> {
        self.backend().migrate(migrations).await
    }

    /// Drop the database now, and report whether that worked. The server ends every connection to it
    /// (`WITH (FORCE)`), including one the test still holds: the pool is not asked to close first,
    /// because closing waits for every checked-out connection to come back.
    pub async fn drop_database(mut self) -> Result<(), RelError> {
        let dropped = drop_named(&self.admin, &self.name).await;
        self.backend.take();
        dropped
    }
}

/// Drop `name` over a connection of its own: a pooled connection belongs to the runtime that made the
/// pool, and the drop path below runs on another one.
async fn drop_named(admin: &PgConnectOptions, name: &str) -> Result<(), RelError> {
    let mut conn = admin
        .connect()
        .await
        .map_err(|e| failed("connect to drop the test database", e))?;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "DROP DATABASE IF EXISTS {name} WITH (FORCE)"
    )))
    .execute(&mut conn)
    .await
    .map(|_| ())
    .map_err(|e| failed("drop the test database", e))
}

impl Drop for TestDb {
    fn drop(&mut self) {
        // `drop_database` already did it.
        let Some(backend) = self.backend.take() else {
            return;
        };
        // The pool is not closed here (that needs its runtime, which may be the one blocked in this
        // call); `WITH (FORCE)` ends its connections on the server, and the pool is dropped with it.
        drop(backend);
        let (admin, name) = (self.admin.clone(), self.name.clone());
        // The test's own runtime may be shutting down (a panic unwinds through here), so the cleanup
        // gets a thread and a runtime of its own and is waited for: a database dropped after the
        // test binary exited is a database that was not dropped.
        let cleanup = std::thread::spawn(move || {
            let Ok(rt) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                return;
            };
            rt.block_on(async {
                let _ = drop_named(&admin, &name).await;
            });
        });
        let _ = cleanup.join();
    }
}
