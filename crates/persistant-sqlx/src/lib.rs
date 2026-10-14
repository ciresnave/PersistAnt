// SPDX-License-Identifier: MIT OR Apache-2.0
//! PersistAnt's relational driver over sqlx (PostgreSQL first).
//!
//! [`PgBackend`] implements [`persistant_relational::RelationalBackend`]. The concrete
//! [`sqlx::PgPool`] is available through [`PgBackend::pool`], so a program that wants
//! compile-time-checked `sqlx::query!` keeps it (D1, C+): this crate does not hide sqlx.
//!
//! Failures are classified from the server's SQLSTATE into [`persistant_relational::RelError`].

mod migrate;

pub use migrate::{MigrateError, MigrateReport, Migration};

use persistant_relational::{Dialect, RelError, RelationalBackend, RelationalTx, Row, Value};
use sqlx::postgres::{PgArguments, PgPoolOptions, PgRow};
use sqlx::query::Query;
use sqlx::{Column, Postgres, Row as SqlxRow, TypeInfo, ValueRef};

/// Classify an sqlx error by SQLSTATE (https://www.postgresql.org/docs/current/errcodes-appendix.html).
pub(crate) fn classify(e: sqlx::Error) -> RelError {
    match &e {
        sqlx::Error::Database(db) => {
            let constraint = db.constraint().map(str::to_string);
            match db.code().as_deref() {
                Some("23505") => RelError::UniqueViolation { constraint },
                Some("23503") => RelError::ForeignKeyViolation { constraint },
                Some("23514") | Some("23502") => RelError::CheckViolation { constraint },
                Some("40001") => RelError::Serialization,
                Some("40P01") => RelError::Deadlock,
                Some(c) if c.starts_with("08") || c.starts_with("57P") => {
                    RelError::Unavailable(e.to_string())
                }
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
    mut q: Query<'q, Postgres, PgArguments>,
    params: &'q [Value],
) -> Result<Query<'q, Postgres, PgArguments>, RelError> {
    for p in params {
        q = match p {
            // A typed NULL: PostgreSQL needs a type to infer parameter types; text works for the
            // statements this layer issues. A caller needing another NULL type casts in SQL.
            Value::Null => q.bind(Option::<String>::None),
            Value::Bool(b) => q.bind(*b),
            Value::Int(n) => q.bind(*n),
            Value::Float(f) => q.bind(*f),
            Value::Text(s) => q.bind(s.as_str()),
            Value::Bytes(b) => q.bind(b.as_slice()),
            other => {
                return Err(RelError::Unsupported(format!(
                    "parameter {other:?} has no PostgreSQL binding in this driver"
                )));
            }
        };
    }
    Ok(q)
}

fn decode(row: &PgRow) -> Result<Row, RelError> {
    let mut values = Vec::with_capacity(row.columns().len());
    for (i, col) in row.columns().iter().enumerate() {
        let raw = row.try_get_raw(i).map_err(classify)?;
        if raw.is_null() {
            values.push(Value::Null);
            continue;
        }
        let name = col.type_info().name().to_string();
        let v = match name.as_str() {
            "BOOL" => Value::Bool(row.try_get::<bool, _>(i).map_err(classify)?),
            "INT2" => Value::Int(i64::from(row.try_get::<i16, _>(i).map_err(classify)?)),
            "INT4" => Value::Int(i64::from(row.try_get::<i32, _>(i).map_err(classify)?)),
            "INT8" => Value::Int(row.try_get::<i64, _>(i).map_err(classify)?),
            "FLOAT4" => Value::Float(f64::from(row.try_get::<f32, _>(i).map_err(classify)?)),
            "FLOAT8" => Value::Float(row.try_get::<f64, _>(i).map_err(classify)?),
            "TEXT" | "VARCHAR" | "NAME" | "BPCHAR" => {
                Value::Text(row.try_get::<String, _>(i).map_err(classify)?)
            }
            "BYTEA" => Value::Bytes(row.try_get::<Vec<u8>, _>(i).map_err(classify)?),
            other => {
                return Err(RelError::Unsupported(format!(
                    "column {:?} has type {other}, which Value cannot carry; use PgBackend::pool",
                    col.name()
                )));
            }
        };
        values.push(v);
    }
    Ok(Row::new(values))
}

/// A PostgreSQL database behind an sqlx pool.
#[derive(Debug, Clone)]
pub struct PgBackend {
    pool: sqlx::PgPool,
    version: String,
}

impl PgBackend {
    /// Connect with default pool settings and read the server version.
    ///
    /// Prefer [`PgBackend::connect_secret`]: a `&str` URL is one `Debug` print from a log file.
    pub async fn connect(url: &str) -> Result<Self, RelError> {
        let pool = PgPoolOptions::new()
            .max_connections(16)
            .connect(url)
            .await
            .map_err(classify)?;
        Self::from_pool(pool).await
    }

    /// Connect with a connection string held as a [`persistant_relational::ConnectionSecret`]:
    /// resolved here, never printed, never part of an error.
    pub async fn connect_secret(
        secret: &persistant_relational::ConnectionSecret,
    ) -> Result<Self, RelError> {
        let url = secret.resolve()?;
        Self::connect(url.expose()).await
    }

    /// Wrap an existing pool (a program that already owns one keeps it).
    pub async fn from_pool(pool: sqlx::PgPool) -> Result<Self, RelError> {
        let full: String = sqlx::query_scalar("SHOW server_version")
            .fetch_one(&pool)
            .await
            .map_err(classify)?;
        // "16.15 (Debian 16.15-1.pgdg13+2)" -> "16.15"
        let version = full.split_whitespace().next().unwrap_or("").to_string();
        Ok(PgBackend { pool, version })
    }

    /// The concrete pool: for `sqlx::query!` and anything this layer does not carry.
    pub fn pool(&self) -> &sqlx::PgPool {
        &self.pool
    }
}

impl RelationalBackend for PgBackend {
    type Tx<'a> = PgTx<'a>;

    fn backend(&self) -> &str {
        "postgres"
    }

    fn version(&self) -> &str {
        &self.version
    }

    fn dialect(&self) -> Dialect {
        Dialect::Postgres
    }

    async fn execute(&self, sql: &str, params: &[Value]) -> Result<u64, RelError> {
        let q = bind(sqlx::query(sqlx::AssertSqlSafe(sql.to_string())), params)?;
        q.execute(&self.pool)
            .await
            .map(|r| r.rows_affected())
            .map_err(classify)
    }

    async fn query(&self, sql: &str, params: &[Value]) -> Result<Vec<Row>, RelError> {
        let q = bind(sqlx::query(sqlx::AssertSqlSafe(sql.to_string())), params)?;
        let rows = q.fetch_all(&self.pool).await.map_err(classify)?;
        rows.iter().map(decode).collect()
    }

    async fn begin(&self) -> Result<PgTx<'_>, RelError> {
        Ok(PgTx {
            tx: self.pool.begin().await.map_err(classify)?,
        })
    }
}

/// A PostgreSQL transaction.
#[derive(Debug)]
pub struct PgTx<'a> {
    tx: sqlx::Transaction<'a, Postgres>,
}

impl RelationalTx for PgTx<'_> {
    async fn execute(&mut self, sql: &str, params: &[Value]) -> Result<u64, RelError> {
        let q = bind(sqlx::query(sqlx::AssertSqlSafe(sql.to_string())), params)?;
        q.execute(&mut *self.tx)
            .await
            .map(|r| r.rows_affected())
            .map_err(classify)
    }

    async fn query(&mut self, sql: &str, params: &[Value]) -> Result<Vec<Row>, RelError> {
        let q = bind(sqlx::query(sqlx::AssertSqlSafe(sql.to_string())), params)?;
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
