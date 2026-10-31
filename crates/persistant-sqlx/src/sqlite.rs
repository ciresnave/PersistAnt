// SPDX-License-Identifier: MIT OR Apache-2.0
//! SQLite behind an sqlx pool (feature `sqlite`; SQLite is compiled in, so this feature pays for a C
//! build that a PostgreSQL-only program does not).
//!
//! Writes are serialised by SQLite itself. A transaction here starts with `BEGIN IMMEDIATE`, so
//! its reads and writes are one exclusive unit: that is what a row lock gives on a server database.

use persistant_relational::{
    Dialect, Isolation, RelError, RelationalBackend, RelationalTx, Row, Value,
};
use sqlx::query::Query;
use sqlx::sqlite::{
    SqliteArguments, SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteRow,
    SqliteSynchronous,
};
use sqlx::{AssertSqlSafe, Row as SqlxRow, Sqlite, TypeInfo, ValueRef};
use std::path::Path;
use std::time::Duration;

/// Classify an SQLite error by its extended result code
/// (<https://www.sqlite.org/rescode.html>).
fn classify(e: sqlx::Error) -> RelError {
    match &e {
        sqlx::Error::Database(db) => {
            let constraint = db.constraint().map(str::to_string);
            match db.code().as_deref() {
                // SQLITE_CONSTRAINT_UNIQUE, SQLITE_CONSTRAINT_PRIMARYKEY
                Some("2067") | Some("1555") => RelError::UniqueViolation { constraint },
                // SQLITE_CONSTRAINT_FOREIGNKEY
                Some("787") => RelError::ForeignKeyViolation { constraint },
                // SQLITE_CONSTRAINT_CHECK, SQLITE_CONSTRAINT_NOTNULL
                Some("275") | Some("1299") => RelError::CheckViolation { constraint },
                // SQLITE_BUSY, SQLITE_LOCKED: the lock was not obtained inside the timeout
                Some("5") | Some("6") | Some("261") | Some("517") => RelError::Serialization,
                _ => RelError::Backend(e.to_string()),
            }
        }
        sqlx::Error::Io(_) | sqlx::Error::PoolTimedOut | sqlx::Error::PoolClosed => {
            RelError::Unavailable(e.to_string())
        }
        _ => RelError::Backend(e.to_string()),
    }
}

fn bind<'q>(
    mut q: Query<'q, Sqlite, SqliteArguments>,
    params: &'q [Value],
) -> Result<Query<'q, Sqlite, SqliteArguments>, RelError> {
    for p in params {
        q = match p {
            Value::Null => q.bind(Option::<String>::None),
            Value::Bool(b) => q.bind(*b),
            Value::Int(n) => q.bind(*n),
            Value::Float(f) => q.bind(*f),
            Value::Text(s) => q.bind(s.as_str()),
            Value::Bytes(b) => q.bind(b.as_slice()),
            other => {
                return Err(RelError::Unsupported(format!(
                    "parameter {other:?} has no SQLite binding in this driver"
                )));
            }
        };
    }
    Ok(q)
}

/// SQLite types a *value*, not a column, so the type comes from the value itself.
fn decode(row: &SqliteRow) -> Result<Row, RelError> {
    let mut values = Vec::with_capacity(row.len());
    for i in 0..row.len() {
        let raw = row.try_get_raw(i).map_err(classify)?;
        if raw.is_null() {
            values.push(Value::Null);
            continue;
        }
        let name = raw.type_info().name().to_string();
        let v = match name.as_str() {
            "INTEGER" | "INT" | "BIGINT" | "INT8" => {
                Value::Int(row.try_get::<i64, _>(i).map_err(classify)?)
            }
            "BOOLEAN" => Value::Bool(row.try_get::<bool, _>(i).map_err(classify)?),
            "REAL" | "FLOAT" | "DOUBLE" => {
                Value::Float(row.try_get::<f64, _>(i).map_err(classify)?)
            }
            "TEXT" => Value::Text(row.try_get::<String, _>(i).map_err(classify)?),
            "BLOB" => Value::Bytes(row.try_get::<Vec<u8>, _>(i).map_err(classify)?),
            other => {
                return Err(RelError::Unsupported(format!(
                    "column {i} has type {other}, which Value cannot carry; use SqliteBackend::pool"
                )));
            }
        };
        values.push(v);
    }
    Ok(Row::new(values))
}

/// An SQLite database behind an sqlx pool.
#[derive(Debug, Clone)]
pub struct SqliteBackend {
    pool: sqlx::SqlitePool,
    version: String,
    migrating: std::sync::Arc<tokio::sync::Mutex<()>>,
    full_sync: bool,
}

/// Create `path` empty and owner-only if it is not there, so SQLite finds an existing zero-length
/// file (it treats that as a new database) and keeps its permissions. `create_new` never touches an
/// existing file; any failure (the directory is missing, no permission) is left for the open that
/// follows to report in its own words.
///
/// On Unix that is mode `0600`, and SQLite gives the `-wal`, `-shm` and `-journal` files beside it
/// the database file's mode. On Windows it is a protected ACL with one entry for the current user
/// (`owner_only.rs`), and SQLite does **not** copy it: the files beside the database inherit the
/// directory's ACL. With `sidecars` (the WAL profile) the `-wal` and `-shm` files are therefore
/// created the same way first, and SQLite opens them as they are. The rollback `-journal` of the
/// default profile is created and removed by SQLite on every transaction and cannot be prepared.
fn owner_only_if_new(path: &Path, sidecars: bool) -> Result<(), RelError> {
    #[cfg(unix)]
    {
        let _ = sidecars;
        use std::os::unix::fs::OpenOptionsExt;
        let _ = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path);
        Ok(())
    }
    #[cfg(windows)]
    {
        // A file that was just created and cannot be restricted is not left open to the directory's
        // readers: it is removed and the open fails, rather than SQLite creating it unrestricted.
        let make = |p: &Path| -> Result<(), RelError> {
            if std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(p)
                .is_ok()
                && let Err(e) = persistant_core::owner_only::restrict_to_current_user(p)
            {
                let _ = std::fs::remove_file(p);
                return Err(RelError::Backend(format!(
                    "could not make {} owner-only: {e}",
                    p.display()
                )));
            }
            Ok(())
        };
        let fresh = !path.exists();
        make(path)?;
        if sidecars && fresh && path.exists() {
            for suffix in ["-wal", "-shm"] {
                let mut name = path.as_os_str().to_owned();
                name.push(suffix);
                make(Path::new(&name))?;
            }
        }
        Ok(())
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (path, sidecars);
        Ok(())
    }
}

impl SqliteBackend {
    /// Open (creating if missing) the database file at `path`, with a bounded wait for the write lock.
    /// The journal mode is SQLite's default (a rollback journal, measured: `delete`); [`open_durable`](Self::open_durable)
    /// asks for WAL.
    ///
    /// **Creating** means the file only: a missing parent directory is an error, not created. On Unix
    /// a new file is created owner-only (`0600`, less the umask), and SQLite gives its `-wal` and
    /// `-shm` files the database file's mode; SQLite's own default would be `0644` less the umask,
    /// world-readable under the usual `022` (measured on Linux). A file that already exists keeps
    /// the mode its owner gave it. **On Windows** a new file gets a protected ACL with one entry, full
    /// control for the user the process runs as (0.9.43), where it would inherit the directory's; the
    /// rollback journal SQLite creates and removes on every transaction still inherits the directory's
    /// ACL, so restrict the directory too if its contents matter, or use
    /// [`open_durable`](Self::open_durable), whose `-wal` and `-shm` files are restricted as well.
    pub async fn open(path: impl AsRef<Path>) -> Result<Self, RelError> {
        owner_only_if_new(path.as_ref(), false)?;
        let opts = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .busy_timeout(Duration::from_secs(30));
        let pool = SqlitePoolOptions::new()
            .max_connections(8)
            .connect_with(opts)
            .await
            .map_err(classify)?;
        Self::from_pool(pool).await
    }

    /// As [`open`](Self::open), with `journal_mode=WAL` and `synchronous=FULL` set explicitly rather
    /// than left to a default. This profile reports itself as `sqlite-full`, which is the name the
    /// key-value `choose` table declares `Machine` for (guest power loss on ext4, run 39178436801);
    /// [`open`](Self::open) stays `sqlite` and declared `Process`. A pool built elsewhere and handed
    /// to [`from_pool`](Self::from_pool) is never assumed to have this setting.
    pub async fn open_durable(path: impl AsRef<Path>) -> Result<Self, RelError> {
        owner_only_if_new(path.as_ref(), true)?;
        let opts = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Full)
            .busy_timeout(Duration::from_secs(30));
        let pool = SqlitePoolOptions::new()
            .max_connections(8)
            .connect_with(opts)
            .await
            .map_err(classify)?;
        let mut me = Self::from_pool(pool).await?;
        me.full_sync = true;
        Ok(me)
    }

    /// Wrap an existing pool. The pool must reach one database: an in-memory database is a
    /// separate database per connection, so give it `max_connections(1)`.
    pub async fn from_pool(pool: sqlx::SqlitePool) -> Result<Self, RelError> {
        let version: String = sqlx::query_scalar("SELECT sqlite_version()")
            .fetch_one(&pool)
            .await
            .map_err(classify)?;
        Ok(SqliteBackend {
            pool,
            version,
            migrating: Default::default(),
            full_sync: false,
        })
    }

    /// The concrete pool.
    pub fn pool(&self) -> &sqlx::SqlitePool {
        &self.pool
    }

    /// The ledger's rows that succeeded, `(version, checksum)` ascending; empty if there is no ledger
    /// yet. Reads the same rows [`Migrates::migrate`] checks against, so a caller can compare a
    /// directory of migrations with a database without applying anything.
    pub async fn applied_checksums(
        &self,
    ) -> Result<Vec<(i64, Vec<u8>)>, persistant_relational::MigrateError> {
        let mut conn = self
            .pool
            .acquire()
            .await
            .map_err(crate::migrate::backend_error)?;
        crate::migrate::ledger(
            &mut *conn,
            "SELECT COUNT(*) FROM sqlite_master WHERE name = '_sqlx_migrations'",
        )
        .await
    }
}

impl RelationalBackend for SqliteBackend {
    type Tx<'a> = SqliteTx;

    fn backend(&self) -> &str {
        if self.full_sync {
            "sqlite-full"
        } else {
            "sqlite"
        }
    }

    fn version(&self) -> &str {
        &self.version
    }

    fn dialect(&self) -> Dialect {
        Dialect::Sqlite
    }

    async fn execute(&self, sql: &str, params: &[Value]) -> Result<u64, RelError> {
        let q = bind(sqlx::query(AssertSqlSafe(sql.to_string())), params)?;
        q.execute(&self.pool)
            .await
            .map(|r| r.rows_affected())
            .map_err(classify)
    }

    async fn query(&self, sql: &str, params: &[Value]) -> Result<Vec<Row>, RelError> {
        let q = bind(sqlx::query(AssertSqlSafe(sql.to_string())), params)?;
        let rows = q.fetch_all(&self.pool).await.map_err(classify)?;
        rows.iter().map(decode).collect()
    }

    /// Every level is `BEGIN IMMEDIATE`: the one write lock makes each of them serializable.
    async fn begin(&self, _isolation: Isolation) -> Result<SqliteTx, RelError> {
        Ok(SqliteTx {
            tx: self
                .pool
                .begin_with("BEGIN IMMEDIATE")
                .await
                .map_err(classify)?,
        })
    }
}

/// An SQLite write transaction (`BEGIN IMMEDIATE`).
#[derive(Debug)]
pub struct SqliteTx {
    tx: sqlx::Transaction<'static, Sqlite>,
}

impl RelationalTx for SqliteTx {
    async fn execute(&mut self, sql: &str, params: &[Value]) -> Result<u64, RelError> {
        let q = bind(sqlx::query(AssertSqlSafe(sql.to_string())), params)?;
        q.execute(&mut *self.tx)
            .await
            .map(|r| r.rows_affected())
            .map_err(classify)
    }

    async fn query(&mut self, sql: &str, params: &[Value]) -> Result<Vec<Row>, RelError> {
        let q = bind(sqlx::query(AssertSqlSafe(sql.to_string())), params)?;
        let rows = q.fetch_all(&mut *self.tx).await.map_err(classify)?;
        rows.iter().map(decode).collect()
    }

    async fn commit(self) -> Result<(), RelError> {
        self.tx.commit().await.map_err(classify)
    }

    async fn rollback(self) -> Result<(), RelError> {
        self.tx.rollback().await.map_err(classify)
    }
}

impl persistant_relational::Migrates for SqliteBackend {
    async fn migrate(
        &self,
        migrations: &[persistant_relational::Migration],
    ) -> Result<persistant_relational::MigrateReport, persistant_relational::MigrateError> {
        use crate::migrate::run_on_own_connection;
        // sqlx takes no lock on SQLite, so runs through this backend (and its clones) take turns here.
        // A second process migrating the same file is not excluded.
        let _turn = self.migrating.lock().await;
        run_on_own_connection!(
            self.pool,
            migrations,
            classify,
            "SELECT COUNT(*) FROM sqlite_master WHERE name = '_sqlx_migrations'",
            None
        )
    }

    async fn applied_versions(&self) -> Result<Vec<i64>, persistant_relational::MigrateError> {
        use crate::migrate::backend_error;
        let exists: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_master WHERE name = '_sqlx_migrations'",
        )
        .fetch_one(&self.pool)
        .await
        .map_err(backend_error)?;
        if exists == 0 {
            return Ok(Vec::new());
        }
        sqlx::query_scalar("SELECT version FROM _sqlx_migrations WHERE success ORDER BY version")
            .fetch_all(&self.pool)
            .await
            .map_err(backend_error)
    }
}
