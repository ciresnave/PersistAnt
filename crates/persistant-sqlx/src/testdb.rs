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
//! **Cheaper cleanup (ROADMAP row 36).** `DROP DATABASE .. WITH (FORCE)` forces a checkpoint, and many
//! of them at once queue behind it: Humboldt measured 70 s with the drop against 15.7 s without, for 19
//! tests. [`Cleanup::Defer`] (through [`TestDb::create_with`], or `PERSISTANT_TESTDB_CLEANUP=defer` in the
//! environment of [`TestDb::create`]) skips the drop when the handle goes out of scope and leaves the
//! database; [`TestDb::sweep`] then drops, one at a time, every `persistant_test_*` database older than a
//! bound, once, at the start of the test binary or the CI job. [`TestDb::drop_database`] always drops.
//!
//! PostgreSQL only: it is the engine Humboldt tests on. SQLite tests use a temporary file and MySQL a
//! scratch database ([`crate::MysqlBackend::verify_fresh`]).

use crate::PgBackend;
use persistant_relational::{MigrateError, MigrateReport, Migration, RelError};
use sqlx::ConnectOptions;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// What happens to a [`TestDb`]'s database when the handle is dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum Cleanup {
    /// Drop the database (the default).
    #[default]
    Drop,
    /// Leave it for [`TestDb::sweep`]: no drop, so no forced checkpoint per test.
    Defer,
}

impl Cleanup {
    /// The mode a value of `PERSISTANT_TESTDB_CLEANUP` names: `defer` (any case) is [`Defer`](Self::Defer),
    /// anything else, or no value, is [`Drop`](Self::Drop).
    pub fn from_env_value(value: Option<&str>) -> Cleanup {
        match value {
            Some(v) if v.trim().eq_ignore_ascii_case("defer") => Cleanup::Defer,
            _ => Cleanup::Drop,
        }
    }
}

/// Options for [`TestDb::create_with`].
#[derive(Debug, Clone, Copy, Default)]
#[non_exhaustive]
pub struct TestDbOptions {
    /// What dropping the handle does; see [`Cleanup`].
    pub cleanup: Cleanup,
}

impl TestDbOptions {
    /// The defaults, with `cleanup` read from `PERSISTANT_TESTDB_CLEANUP` (what [`TestDb::create`] uses).
    pub fn from_env() -> Self {
        TestDbOptions {
            cleanup: Cleanup::from_env_value(
                std::env::var("PERSISTANT_TESTDB_CLEANUP").ok().as_deref(),
            ),
        }
    }

    /// These options with `cleanup` set.
    pub fn cleanup(mut self, cleanup: Cleanup) -> Self {
        self.cleanup = cleanup;
        self
    }
}

/// A database created for one test and dropped when the test is done.
#[derive(Debug)]
pub struct TestDb {
    admin: PgConnectOptions,
    name: String,
    cleanup: Cleanup,
    // `None` once dropped.
    backend: Option<PgBackend>,
}

fn unique_name() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    format!(
        "persistant_test_{}_{}_{}",
        std::process::id(),
        nanos,
        COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

/// The creation time, in nanoseconds since the epoch, of a name [`unique_name`] made; `None` for any other
/// name (so `sweep` never drops a database it did not create).
fn created_nanos(name: &str) -> Option<u128> {
    let rest = name.strip_prefix("persistant_test_")?;
    let mut parts = rest.split('_');
    let (pid, nanos, counter) = (parts.next()?, parts.next()?, parts.next()?);
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if parts.next().is_some() || !digits(pid) || !digits(counter) || !digits(nanos) {
        return None;
    }
    nanos.parse().ok()
}

fn failed(what: &str, e: sqlx::Error) -> RelError {
    RelError::Backend(format!("{what}: {e}"))
}

impl TestDb {
    /// Create an empty database on the server `admin` is connected to and connect to it (a pool of up
    /// to 8 connections, the admin connection's options otherwise).
    ///
    /// The cleanup mode comes from `PERSISTANT_TESTDB_CLEANUP` ([`TestDbOptions::from_env`]): unset, the
    /// database is dropped with the handle; `defer`, it is left for [`TestDb::sweep`].
    pub async fn create(admin: &PgBackend) -> Result<TestDb, RelError> {
        TestDb::create_with(admin, TestDbOptions::from_env()).await
    }

    /// As [`create`](Self::create), with the cleanup mode given rather than read from the environment.
    pub async fn create_with(
        admin: &PgBackend,
        options: TestDbOptions,
    ) -> Result<TestDb, RelError> {
        let cleanup = options.cleanup;
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
                cleanup,
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

    /// Drop every `persistant_test_*` database on the server `admin` is connected to that is **older than
    /// `older_than`**, one at a time (they would queue on the forced checkpoint anyway), and return how many
    /// went. Run it once, at the start of the test binary or the CI job, with a bound longer than any test
    /// (an hour, say): it cannot tell a leftover from the live database of a binary running beside yours, and
    /// a bound of zero drops those too. Only a name this module made is touched
    /// (`persistant_test_<digits>_<digits>_<digits>`; the middle number is the creation time in nanoseconds
    /// since the epoch, which is the age), so a database someone else named `persistant_test_x` is left alone.
    pub async fn sweep(admin: &PgBackend, older_than: Duration) -> Result<usize, RelError> {
        let rows = sqlx::query_scalar::<_, String>(
            "SELECT datname::text FROM pg_database WHERE datname LIKE 'persistant\\_test\\_%'",
        )
        .fetch_all(admin.pool())
        .await
        .map_err(|e| failed("list the test databases", e))?;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let admin_options = (*admin.pool().connect_options()).clone();
        let mut dropped = 0;
        for name in rows {
            let Some(created) = created_nanos(&name) else {
                continue;
            };
            if now.saturating_sub(created) > older_than.as_nanos() {
                drop_named(&admin_options, &name).await?;
                dropped += 1;
            }
        }
        Ok(dropped)
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
        // `Defer`: leave the database for `sweep`.
        if self.cleanup == Cleanup::Defer {
            return;
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_name_this_module_made_has_an_age() {
        assert_eq!(created_nanos("persistant_test_12_3456_7"), Some(3456));
        let made = unique_name();
        assert!(created_nanos(&made).is_some(), "{made}");
        for other in [
            "persistant_test_x",
            "persistant_test_1_abc_0",
            "persistant_test_1_2",
            "persistant_test_1_2_3_4",
            "persistant_test__2_3",
            "persistant_prod_1_2_3",
            "postgres",
        ] {
            assert_eq!(created_nanos(other), None, "{other}");
        }
    }

    #[test]
    fn the_environment_value_picks_the_mode() {
        for (v, want) in [
            (Some("defer"), Cleanup::Defer),
            (Some(" DEFER "), Cleanup::Defer),
            (Some("drop"), Cleanup::Drop),
            (Some(""), Cleanup::Drop),
            (None, Cleanup::Drop),
        ] {
            assert_eq!(Cleanup::from_env_value(v), want, "{v:?}");
        }
        assert_eq!(TestDbOptions::default().cleanup, Cleanup::Drop);
    }
}
