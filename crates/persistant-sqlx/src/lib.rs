// SPDX-License-Identifier: MIT OR Apache-2.0
//! PersistAnt's relational driver over sqlx (PostgreSQL first).
//!
//! [`PgBackend`] implements [`persistant_relational::RelationalBackend`]. The concrete
//! [`sqlx::PgPool`] is available through [`PgBackend::pool`], so a program that wants
//! compile-time-checked `sqlx::query!` keeps it (D1, C+): this crate does not hide sqlx.
//!
//! Failures are classified from the server's SQLSTATE into [`persistant_relational::RelError`].

mod bounded;
#[cfg(feature = "bridge")]
mod bridge;
mod decode;
mod migrate;
#[cfg(feature = "mysql")]
mod mysql;
mod options;
#[cfg(feature = "sqlite")]
mod sqlite;

pub use bounded::{Bounded, BoundedTx, call_bound_from_env, parse_bound};
#[cfg(feature = "bridge")]
pub use bridge::{OnConn, OnPool};
pub use migrate::{FromSqlxError, migrations_from_sqlx};
#[cfg(feature = "mysql")]
pub use mysql::{MysqlBackend, MysqlTx};
pub use options::{ConnectOptions, TlsPolicy};
pub use persistant_relational::{MigrateError, MigrateReport, Migrates, Migration};
#[cfg(feature = "sqlite")]
pub use sqlite::{SqliteBackend, SqliteTx};

use persistant_relational::{
    Dialect, Isolation, RelError, RelationalBackend, RelationalTx, Row, Value,
};
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

/// A `NULL` whose type the server infers from the statement. Binding `Option::<String>::None` sends
/// the type `text`, which the server rejects for a `bigint` column (`expression is of type text`) the
/// first time a pooled connection prepares that statement, and not on one that already has: a failure
/// that depends on which connection the pool hands out. Type OID 0 means "unspecified".
struct UntypedNull;

impl sqlx::Type<Postgres> for UntypedNull {
    fn type_info() -> sqlx::postgres::PgTypeInfo {
        sqlx::postgres::PgTypeInfo::with_oid(sqlx::postgres::types::Oid(0))
    }

    fn compatible(_: &sqlx::postgres::PgTypeInfo) -> bool {
        true
    }
}

impl<'q> sqlx::Encode<'q, Postgres> for UntypedNull {
    fn encode_by_ref(
        &self,
        _: &mut sqlx::postgres::PgArgumentBuffer,
    ) -> Result<sqlx::encode::IsNull, sqlx::error::BoxDynError> {
        Ok(sqlx::encode::IsNull::Yes)
    }
}

fn bind<'q>(
    mut q: Query<'q, Postgres, PgArguments>,
    params: &'q [Value],
) -> Result<Query<'q, Postgres, PgArguments>, RelError> {
    for p in params {
        q = match p {
            // A NULL of unspecified type: the server infers it from the statement.
            Value::Null => q.bind(UntypedNull),
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

fn decode_failed(e: sqlx::error::BoxDynError) -> RelError {
    RelError::Backend(e.to_string())
}

fn malformed(column: &str, ty: &str, why: String) -> RelError {
    RelError::Backend(format!("column {column:?} ({ty}): {why}"))
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
            "FLOAT4" => Value::Float(decode::f32_shortest(
                row.try_get::<f32, _>(i).map_err(classify)?,
            )),
            "FLOAT8" => Value::Float(row.try_get::<f64, _>(i).map_err(classify)?),
            // sqlx names `bpchar` (char(n)) "CHAR"; the quoted "CHAR" is the one-byte internal type.
            "TEXT" | "VARCHAR" | "NAME" | "BPCHAR" | "CHAR" => {
                Value::Text(row.try_get::<String, _>(i).map_err(classify)?)
            }
            "BYTEA" => Value::Bytes(row.try_get::<Vec<u8>, _>(i).map_err(classify)?),
            // Types with a canonical text form (`decode.rs`): carried as that text.
            "UUID" => Value::Text(
                decode::uuid_text(raw.as_bytes().map_err(decode_failed)?)
                    .map_err(|m| malformed(col.name(), &name, m))?,
            ),
            "NUMERIC" => Value::Text(
                decode::numeric_text(raw.as_bytes().map_err(decode_failed)?)
                    .map_err(|m| malformed(col.name(), &name, m))?,
            ),
            "INET" | "CIDR" => Value::Text(
                decode::inet_text(raw.as_bytes().map_err(decode_failed)?)
                    .map_err(|m| malformed(col.name(), &name, m))?,
            ),
            "JSON" => Value::Text(
                String::from_utf8(raw.as_bytes().map_err(decode_failed)?.to_vec())
                    .map_err(|e| malformed(col.name(), &name, e.to_string()))?,
            ),
            "JSONB" => {
                // Version byte 1, then the JSON text.
                let b = raw.as_bytes().map_err(decode_failed)?;
                match b.split_first() {
                    Some((1, rest)) => Value::Text(
                        String::from_utf8(rest.to_vec())
                            .map_err(|e| malformed(col.name(), &name, e.to_string()))?,
                    ),
                    _ => return Err(malformed(col.name(), &name, "unknown jsonb version".into())),
                }
            }
            "DATE" => Value::Text(decode::date_text(
                row.try_get::<sqlx::types::time::Date, _>(i)
                    .map_err(classify)?,
            )),
            "TIME" => Value::Text(decode::time_text(
                row.try_get::<sqlx::types::time::Time, _>(i)
                    .map_err(classify)?,
            )),
            "TIMESTAMP" => Value::Text(decode::naive_text(
                row.try_get::<sqlx::types::time::PrimitiveDateTime, _>(i)
                    .map_err(classify)?,
            )),
            "TIMESTAMPTZ" => Value::Text(decode::utc_text(
                row.try_get::<sqlx::types::time::OffsetDateTime, _>(i)
                    .map_err(classify)?,
            )),
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

    async fn begin(&self, isolation: Isolation) -> Result<PgTx<'_>, RelError> {
        let statement = match isolation {
            Isolation::ReadCommitted => "BEGIN",
            Isolation::RepeatableRead => "BEGIN ISOLATION LEVEL REPEATABLE READ",
            Isolation::Serializable => "BEGIN ISOLATION LEVEL SERIALIZABLE",
            other => return Err(RelError::Unsupported(format!("isolation level {other:?}"))),
        };
        Ok(PgTx {
            tx: self.pool.begin_with(statement).await.map_err(classify)?,
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
