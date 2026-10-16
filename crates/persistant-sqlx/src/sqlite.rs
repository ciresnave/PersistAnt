// SPDX-License-Identifier: MIT OR Apache-2.0
//! SQLite behind an sqlx pool (feature `sqlite`; SQLite is compiled in, so this feature pays for a C
//! build that a PostgreSQL-only program does not).
//!
//! Writes are serialised by SQLite itself. A transaction here starts with `BEGIN IMMEDIATE`, so
//! its reads and writes are one exclusive unit: that is what a row lock gives on a server database.

use persistant_relational::{Dialect, RelError, RelationalBackend, RelationalTx, Row, Value};
use sqlx::query::Query;
use sqlx::sqlite::{SqliteArguments, SqliteConnectOptions, SqlitePoolOptions, SqliteRow};
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
}

impl SqliteBackend {
    /// Open (creating if missing) the database file at `path`, in WAL mode, with a bounded wait
    /// for the write lock.
    pub async fn open(path: impl AsRef<Path>) -> Result<Self, RelError> {
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

    /// Wrap an existing pool. The pool must reach one database: an in-memory database is a
    /// separate database per connection, so give it `max_connections(1)`.
    pub async fn from_pool(pool: sqlx::SqlitePool) -> Result<Self, RelError> {
        let version: String = sqlx::query_scalar("SELECT sqlite_version()")
            .fetch_one(&pool)
            .await
            .map_err(classify)?;
        Ok(SqliteBackend { pool, version })
    }

    /// The concrete pool.
    pub fn pool(&self) -> &sqlx::SqlitePool {
        &self.pool
    }
}

impl RelationalBackend for SqliteBackend {
    type Tx<'a> = SqliteTx;

    fn backend(&self) -> &str {
        "sqlite"
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

    async fn begin(&self) -> Result<SqliteTx, RelError> {
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
