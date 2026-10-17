// SPDX-License-Identifier: MIT OR Apache-2.0
//! Checksummed, ordered, resumable schema migrations that **keep sqlx's ledger**.
//!
//! The ledger is sqlx's own `_sqlx_migrations` table, written by sqlx's own `Migrator`, so its
//! shape and its checksums (SHA-384 of the migration text) are exactly what `sqlx::migrate!` and
//! `sqlx migrate run` write and read. That is a constraint, not a preference: Humboldt's transfer
//! tool compares versions *and* checksums from that table (`docs/DESIGN-KINDS.md` §5), and a
//! database can be handled by either this crate or sqlx's CLI without a conversion.
//!
//! What this layer adds is a typed outcome instead of an sqlx error string, and a report of what it
//! applied. The types and the [`Migrates`] trait live in `persistant-relational`; this module
//! implements it for each backend, and `persistant_relational::migrate_conformance` decides what the
//! implementation actually promises.

use crate::{PgBackend, classify};
use persistant_relational::{MigrateError, MigrateReport, Migrates, Migration, RelError};
use sqlx::SqlSafeStr;
use sqlx::migrate::{Migration as SqlxMigration, MigrationType, Migrator};

/// The program's migrations as sqlx's own `Migrator`.
pub(crate) fn migrator(migrations: &[Migration]) -> Migrator {
    let sqlx_migrations: Vec<SqlxMigration> = migrations
        .iter()
        .map(|m| {
            SqlxMigration::new(
                m.version,
                m.description.clone().into(),
                MigrationType::Simple,
                sqlx::AssertSqlSafe(m.sql.clone()).into_sql_str(),
                false,
            )
        })
        .collect();
    Migrator::with_migrations(sqlx_migrations)
}

/// Why an sqlx [`Migrator`] cannot be run by [`Migrates::migrate`] as it stands.
///
/// Each variant is something sqlx would honour and this crate would not: refusing is the point, so
/// a program never believes it ran `sqlx::migrate!` and got a different run.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum FromSqlxError {
    /// The migration asks to run outside a transaction (`-- no-transaction`, or `no_tx` on the
    /// `Migrator`); [`Migration`] always runs in one.
    NoTransaction {
        /// The version.
        version: i64,
    },
    /// The `Migrator` keeps its ledger in another table than `_sqlx_migrations`, so this crate would
    /// read and write a different ledger than the program asked for.
    LedgerTable {
        /// The table the `Migrator` names.
        table: String,
    },
    /// The `Migrator` was set to create a schema or to skip locking; this crate always locks and never
    /// creates a schema.
    Setting(&'static str),
    /// A migration's stored checksum is not the SHA-384 of its text, so a ledger written by this
    /// crate would differ from the one sqlx writes for the same migration.
    Checksum {
        /// The version.
        version: i64,
    },
}

impl std::fmt::Display for FromSqlxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FromSqlxError::NoTransaction { version } => {
                write!(f, "migration {version} runs outside a transaction")
            }
            FromSqlxError::LedgerTable { table } => {
                write!(
                    f,
                    "the migrator keeps its ledger in `{table}`, not `_sqlx_migrations`"
                )
            }
            FromSqlxError::Setting(s) => write!(f, "the migrator sets {s}, which is not supported"),
            FromSqlxError::Checksum { version } => {
                write!(
                    f,
                    "migration {version}: checksum is not the SHA-384 of its text"
                )
            }
        }
    }
}

impl std::error::Error for FromSqlxError {}

/// The migrations of an sqlx [`Migrator`] (`sqlx::migrate!("./migrations")` or `Migrator::new(path)`)
/// as [`Migration`]s for [`Migrates::migrate`], in the order sqlx would run them.
///
/// The ledger is shared, not converted: [`Migrates::migrate`] writes sqlx's own `_sqlx_migrations`
/// with the same SHA-384 checksum sqlx computes from the same text, so a database migrated by
/// `sqlx migrate run` is accepted as it is (and the other way round). Down halves of reversible
/// migrations are dropped (this crate never undoes). Anything sqlx would do differently from this
/// crate is an error, not a silent difference: see [`FromSqlxError`].
pub fn migrations_from_sqlx(migrator: &Migrator) -> Result<Vec<Migration>, FromSqlxError> {
    if migrator.table_name != "_sqlx_migrations" {
        return Err(FromSqlxError::LedgerTable {
            table: migrator.table_name.to_string(),
        });
    }
    if migrator.no_tx {
        return Err(FromSqlxError::Setting("no_tx"));
    }
    if !migrator.locking {
        return Err(FromSqlxError::Setting("locking = false"));
    }
    let mut out = Vec::new();
    for m in migrator
        .iter()
        .filter(|m| !m.migration_type.is_down_migration())
    {
        if m.no_tx {
            return Err(FromSqlxError::NoTransaction { version: m.version });
        }
        let sql = sqlx::SqlStr::as_str(&m.sql);
        let ours = Migration::new(m.version, m.description.to_string(), sql);
        if migrator_checksum(&ours) != m.checksum.as_ref() {
            return Err(FromSqlxError::Checksum { version: m.version });
        }
        out.push(ours);
    }
    Ok(out)
}

/// The checksum this crate's ledger will hold for `m`: computed by sqlx's own constructor.
fn migrator_checksum(m: &Migration) -> Vec<u8> {
    migrator(std::slice::from_ref(m))
        .iter()
        .next()
        .map(|x| x.checksum.to_vec())
        .unwrap_or_default()
}

/// sqlx's migration error as a typed one; `classify` is the backend's own error classification.
pub(crate) fn map_error(
    e: sqlx::migrate::MigrateError,
    classify: fn(sqlx::Error) -> RelError,
) -> MigrateError {
    match e {
        sqlx::migrate::MigrateError::VersionMismatch(version) => {
            MigrateError::ChecksumMismatch { version }
        }
        sqlx::migrate::MigrateError::VersionMissing(version) => {
            MigrateError::NewerThanProgram { version }
        }
        sqlx::migrate::MigrateError::Dirty(version) => MigrateError::Dirty { version },
        sqlx::migrate::MigrateError::ExecuteMigration(e, version) => MigrateError::Failed {
            version,
            error: classify(e),
        },
        other => MigrateError::Backend(other.to_string()),
    }
}

pub(crate) fn backend_error(e: sqlx::Error) -> MigrateError {
    MigrateError::Backend(e.to_string())
}

/// Run the migrations on one connection taken from the pool and report what **this run** applied.
/// sqlx holds a session advisory lock for the length of a run and does **not** release it when a
/// migration fails, so the pooled connection would keep it and the next run, on another connection, would
/// wait for it forever (measured on MySQL 8.4: a hang after a failed migration). The lock belongs to the
/// session, so after a failure the session is ended instead of returned to the pool.
///
/// The ledger is read before and after on the same connection, **inside the lock** (the advisory lock
/// stacks, so sqlx taking it again is harmless). Read outside it, two simultaneous runs both see the
/// ledger empty at the start and full at the end, and both report every version as theirs. `$exists` is a
/// statement that counts the ledger table (0 or 1).
///
/// `$guard` is `Some((take, release))` where sqlx's own lock is not a lock. MariaDB rejects the negative
/// timeout sqlx passes to `GET_LOCK(.., -1)` and returns NULL, which sqlx ignores, so two runs overlap
/// (measured on MariaDB 11.4: `Duplicate entry '2' for key 'PRIMARY'`). `take` is a statement returning 1
/// once the lock is held; `release` frees it.
macro_rules! run_on_own_connection {
    ($pool:expr, $migrations:expr, $classify:expr, $exists:expr, $guard:expr) => {{
        let mut conn = $pool
            .acquire()
            .await
            .map_err(crate::migrate::backend_error)?;
        let mut failure = None;
        let mut before = Vec::new();
        let mut after = Vec::new();
        let guard: Option<(&str, &str)> = $guard;
        'run: {
            if let Some((take, _)) = guard {
                match sqlx::query_scalar::<_, Option<i64>>(sqlx::AssertSqlSafe(take.to_string()))
                    .fetch_one(&mut *conn)
                    .await
                {
                    Ok(Some(1)) => {}
                    Ok(other) => {
                        failure = Some(persistant_relational::MigrateError::Backend(format!(
                            "the migration lock was not granted: {other:?}"
                        )));
                        break 'run;
                    }
                    Err(e) => {
                        failure = Some(crate::migrate::backend_error(e));
                        break 'run;
                    }
                }
            }
            if let Err(e) = sqlx::migrate::Migrate::lock(&mut *conn).await {
                failure = Some(persistant_relational::MigrateError::Backend(e.to_string()));
                break 'run;
            }
            match crate::migrate::ledger(&mut *conn, $exists).await {
                Ok(v) => before = v.into_iter().map(|(version, _)| version).collect(),
                Err(e) => {
                    failure = Some(e);
                    break 'run;
                }
            }
            // `run_direct` is sqlx own way round "implementation of `Acquire` is not general enough".
            if let Err(e) = crate::migrate::migrator($migrations)
                .run_direct(None, &mut *conn, false)
                .await
            {
                failure = Some(crate::migrate::map_error(e, $classify));
                break 'run;
            }
            match crate::migrate::ledger(&mut *conn, $exists).await {
                Ok(v) => after = v,
                Err(e) => {
                    failure = Some(e);
                    break 'run;
                }
            }
            if let Err(e) = sqlx::migrate::Migrate::unlock(&mut *conn).await {
                failure = Some(persistant_relational::MigrateError::Backend(e.to_string()));
            }
            if let Some((_, release)) = guard {
                if let Err(e) = sqlx::query(sqlx::AssertSqlSafe(release.to_string()))
                    .execute(&mut *conn)
                    .await
                {
                    failure = Some(crate::migrate::backend_error(e));
                }
            }
        }
        match failure {
            None => {
                let applied = after
                    .iter()
                    .map(|(version, _)| *version)
                    .filter(|v| !before.contains(v))
                    .collect();
                let checksums = after;
                Ok(persistant_relational::MigrateReport {
                    applied,
                    already_applied: before,
                    checksums,
                })
            }
            Some(e) => {
                let _ = sqlx::Connection::close(conn.detach()).await;
                Err(e)
            }
        }
    }};
}
#[cfg(any(feature = "mysql", feature = "sqlite"))]
pub(crate) use run_on_own_connection;

/// The versions the ledger says succeeded, read on `conn`; empty if there is no ledger. `exists` is a
/// statement that returns 1 when the ledger table is there and 0 when it is not.
pub(crate) async fn ledger<C>(
    conn: &mut C,
    exists: &str,
) -> Result<Vec<(i64, Vec<u8>)>, MigrateError>
where
    C: sqlx::Connection,
    for<'c> &'c mut C: sqlx::Executor<'c, Database = C::Database>,
    for<'q> i64: sqlx::Decode<'q, C::Database> + sqlx::Type<C::Database>,
    for<'q> Vec<u8>: sqlx::Decode<'q, C::Database> + sqlx::Type<C::Database>,
    <C::Database as sqlx::Database>::Arguments: sqlx::IntoArguments<C::Database>,
    usize: sqlx::ColumnIndex<<C::Database as sqlx::Database>::Row>,
{
    let there: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(exists.to_string()))
        .fetch_one(&mut *conn)
        .await
        .map_err(backend_error)?;
    if there == 0 {
        return Ok(Vec::new());
    }
    sqlx::query_as(sqlx::AssertSqlSafe(
        "SELECT version, checksum FROM _sqlx_migrations WHERE success ORDER BY version".to_string(),
    ))
    .fetch_all(&mut *conn)
    .await
    .map_err(backend_error)
}

impl Migrates for PgBackend {
    async fn migrate(&self, migrations: &[Migration]) -> Result<MigrateReport, MigrateError> {
        run_on_own_connection!(
            self.pool,
            migrations,
            classify,
            "SELECT CASE WHEN to_regclass('_sqlx_migrations') IS NULL THEN 0::int8 ELSE 1::int8 END",
            None
        )
    }

    async fn applied_versions(&self) -> Result<Vec<i64>, MigrateError> {
        let exists: bool = sqlx::query_scalar("SELECT to_regclass('_sqlx_migrations') IS NOT NULL")
            .fetch_one(&self.pool)
            .await
            .map_err(backend_error)?;
        if !exists {
            return Ok(Vec::new());
        }
        sqlx::query_scalar("SELECT version FROM _sqlx_migrations WHERE success ORDER BY version")
            .fetch_all(&self.pool)
            .await
            .map_err(backend_error)
    }
}

impl PgBackend {
    /// Apply `migrations` in version order, skipping those already applied, in the ledger
    /// `_sqlx_migrations` of the connection's current schema. See [`Migrates::migrate`].
    pub async fn migrate(&self, migrations: &[Migration]) -> Result<MigrateReport, MigrateError> {
        Migrates::migrate(self, migrations).await
    }

    /// Prove that `migrations` install on an **empty** schema, in order, and leave nothing behind.
    ///
    /// A scratch schema with a unique name is created, the migrations run in it on a pool of one
    /// connection whose `search_path` is that schema (so the ledger is the scratch schema's own), and
    /// the schema is dropped afterwards, whether the run passed or failed. The report lists every
    /// version as applied. The database being installed to is never touched: not its ledger, not its
    /// tables.
    ///
    /// Limits: a migration that names another schema explicitly, or creates an extension, role or
    /// other cluster-wide object, escapes the scratch schema; that is not detected. This is the
    /// PostgreSQL driver only.
    pub async fn verify_fresh(
        &self,
        migrations: &[Migration],
    ) -> Result<MigrateReport, MigrateError> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let schema = format!(
            "persistant_fresh_{}_{}_{}",
            std::process::id(),
            nanos,
            N.fetch_add(1, Ordering::Relaxed)
        );
        sqlx::query(sqlx::AssertSqlSafe(format!("CREATE SCHEMA {schema}")))
            .execute(&self.pool)
            .await
            .map_err(backend_error)?;
        let search = format!("SET search_path TO {schema}");
        let scratch = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .after_connect(move |conn, _| {
                let search = search.clone();
                Box::pin(async move {
                    sqlx::query(sqlx::AssertSqlSafe(search))
                        .execute(&mut *conn)
                        .await
                        .map(|_| ())
                })
            })
            .connect_with((*self.pool.connect_options()).clone())
            .await;
        let result = match scratch {
            Ok(pool) => {
                let r = match PgBackend::from_pool(pool.clone()).await {
                    Ok(b) => Migrates::migrate(&b, migrations).await,
                    Err(e) => Err(MigrateError::Backend(e.to_string())),
                };
                pool.close().await;
                r
            }
            Err(e) => Err(backend_error(e)),
        };
        let dropped = sqlx::query(sqlx::AssertSqlSafe(format!("DROP SCHEMA {schema} CASCADE")))
            .execute(&self.pool)
            .await
            .map_err(backend_error);
        let report = result?;
        dropped?;
        Ok(report)
    }

    /// The versions the ledger says succeeded, ascending; empty if there is no ledger yet.
    pub async fn applied_versions(&self) -> Result<Vec<i64>, MigrateError> {
        Migrates::applied_versions(self).await
    }
}
