// SPDX-License-Identifier: MIT OR Apache-2.0
//! MySQL (and MariaDB) behind an sqlx pool (feature `mysql`).
//!
//! The dialect is read from the server's version string. Placeholders are positional `?`.
//! **Rows affected**: sqlx always connects with `CLIENT_FOUND_ROWS`, so an `UPDATE` reports rows
//! *matched* and an upsert that changes nothing reports 1, the same as an insert (measured on MySQL
//! 8.4; `ROW_COUNT()` follows the same flag). A caller that must tell them apart does not use an
//! upsert's count: `KvOnRelational::claim_two_step` is written around that.
//! TLS: [`MysqlBackend::connect_with`] takes a typed [`crate::TlsPolicy`] that overrides the URL's
//! `ssl-mode`. `connect` and `connect_secret` leave the URL's `ssl-mode` in charge (sqlx's default
//! is `PREFERRED`, a silent downgrade). `VerifyCa` maps to `VERIFY_CA`, `VerifyFull` to
//! `VERIFY_IDENTITY`.

use crate::{ConnectOptions, TlsPolicy};
use persistant_relational::{
    Dialect, Isolation, RelError, RelationalBackend, RelationalTx, Row, Value,
};
use sqlx::mysql::{MySqlArguments, MySqlConnectOptions, MySqlPoolOptions, MySqlRow, MySqlSslMode};
use sqlx::query::Query;
use sqlx::{AssertSqlSafe, MySql, Row as SqlxRow, TypeInfo, ValueRef};
use std::str::FromStr;

/// Classify an sqlx error by MySQL's error number
/// (<https://dev.mysql.com/doc/mysql-errors/8.4/en/server-error-reference.html>).
fn classify(e: sqlx::Error) -> RelError {
    match &e {
        sqlx::Error::Database(db) => {
            let number = db
                .try_downcast_ref::<sqlx::mysql::MySqlDatabaseError>()
                .map(|m| m.number());
            let constraint = db.constraint().map(str::to_string);
            match number {
                // ER_DUP_ENTRY, ER_DUP_ENTRY_WITH_KEY_NAME
                Some(1062) | Some(1586) => RelError::UniqueViolation { constraint },
                // ER_NO_REFERENCED_ROW_2, ER_ROW_IS_REFERENCED_2
                Some(1452) | Some(1451) => RelError::ForeignKeyViolation { constraint },
                // ER_CHECK_CONSTRAINT_VIOLATED (MySQL), ER_CONSTRAINT_FAILED (MariaDB), ER_BAD_NULL_ERROR
                Some(3819) | Some(4025) | Some(1048) => RelError::CheckViolation { constraint },
                // ER_LOCK_DEADLOCK
                Some(1213) => RelError::Deadlock,
                // ER_LOCK_WAIT_TIMEOUT
                Some(1205) => RelError::Serialization,
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
    mut q: Query<'q, MySql, MySqlArguments>,
    params: &'q [Value],
) -> Result<Query<'q, MySql, MySqlArguments>, RelError> {
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
                    "parameter {other:?} has no MySQL binding in this driver"
                )));
            }
        };
    }
    Ok(q)
}

fn decode(row: &MySqlRow) -> Result<Row, RelError> {
    let mut values = Vec::with_capacity(row.len());
    for i in 0..row.len() {
        let raw = row.try_get_raw(i).map_err(classify)?;
        if raw.is_null() {
            values.push(Value::Null);
            continue;
        }
        let name = raw.type_info().name().to_string();
        let v = match name.as_str() {
            // sqlx names every TINYINT(1) "BOOLEAN", but MySQL keeps any byte there: 0 and 1 are a
            // boolean, 2..=127 are that integer, and a byte of 128 or more could be signed or
            // unsigned, which the column's name does not say: refused rather than guessed.
            "BOOLEAN" => match row.try_get_unchecked::<u8, _>(i).map_err(classify)? {
                0 => Value::Bool(false),
                1 => Value::Bool(true),
                n @ 2..=127 => Value::Int(i64::from(n)),
                n => {
                    return Err(RelError::Unsupported(format!(
                        "column {i} has type TINYINT(1) and holds byte {n}, which is signed or                          unsigned and cannot be told apart here; use MysqlBackend::pool"
                    )));
                }
            },
            "TINYINT" => Value::Int(i64::from(row.try_get::<i8, _>(i).map_err(classify)?)),
            "TINYINT UNSIGNED" => Value::Int(i64::from(row.try_get::<u8, _>(i).map_err(classify)?)),
            "SMALLINT UNSIGNED" => {
                Value::Int(i64::from(row.try_get::<u16, _>(i).map_err(classify)?))
            }
            "MEDIUMINT UNSIGNED" | "INT UNSIGNED" => {
                Value::Int(i64::from(row.try_get::<u32, _>(i).map_err(classify)?))
            }
            // Above i64::MAX there is no Value::Int: refused, never wrapped to a negative number.
            "BIGINT UNSIGNED" => {
                let v = row.try_get::<u64, _>(i).map_err(classify)?;
                Value::Int(i64::try_from(v).map_err(|_| {
                    RelError::Unsupported(format!(
                        "column {i} has type BIGINT UNSIGNED and holds {v}, which is above i64::MAX;                          use MysqlBackend::pool"
                    ))
                })?)
            }
            "YEAR" => Value::Int(row.try_get_unchecked::<i64, _>(i).map_err(classify)?),
            // Types with a canonical text form (`decode.rs`).
            "DECIMAL" | "ENUM" | "JSON" => {
                Value::Text(row.try_get_unchecked::<String, _>(i).map_err(classify)?)
            }
            "DATE" => Value::Text(crate::decode::date_text(
                row.try_get::<sqlx::types::time::Date, _>(i)
                    .map_err(classify)?,
            )),
            "DATETIME" => Value::Text(crate::decode::naive_text(
                row.try_get::<sqlx::types::time::PrimitiveDateTime, _>(i)
                    .map_err(classify)?,
            )),
            "TIMESTAMP" => Value::Text(crate::decode::utc_text(
                row.try_get::<sqlx::types::time::OffsetDateTime, _>(i)
                    .map_err(classify)?,
            )),
            "TIME" => Value::Text(crate::decode::mysql_time_text(
                &row.try_get::<sqlx::mysql::types::MySqlTime, _>(i)
                    .map_err(classify)?,
            )),
            "SMALLINT" => Value::Int(i64::from(row.try_get::<i16, _>(i).map_err(classify)?)),
            "INT" | "MEDIUMINT" => {
                Value::Int(i64::from(row.try_get::<i32, _>(i).map_err(classify)?))
            }
            "BIGINT" => Value::Int(row.try_get::<i64, _>(i).map_err(classify)?),
            "FLOAT" => Value::Float(crate::decode::f32_shortest(
                row.try_get::<f32, _>(i).map_err(classify)?,
            )),
            "DOUBLE" => Value::Float(row.try_get::<f64, _>(i).map_err(classify)?),
            "CHAR" | "VARCHAR" | "TEXT" | "TINYTEXT" | "MEDIUMTEXT" | "LONGTEXT" => {
                Value::Text(row.try_get::<String, _>(i).map_err(classify)?)
            }
            "BINARY" | "VARBINARY" | "BLOB" | "TINYBLOB" | "MEDIUMBLOB" | "LONGBLOB" => {
                Value::Bytes(row.try_get::<Vec<u8>, _>(i).map_err(classify)?)
            }
            other => {
                return Err(RelError::Unsupported(format!(
                    "column {i} has type {other}, which Value cannot carry; use MysqlBackend::pool"
                )));
            }
        };
        values.push(v);
    }
    Ok(Row::new(values))
}

/// A MySQL or MariaDB database behind an sqlx pool.
#[derive(Debug, Clone)]
pub struct MysqlBackend {
    pool: sqlx::MySqlPool,
    version: String,
    dialect: Dialect,
}

impl MysqlBackend {
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
            "SELECT COUNT(*) FROM information_schema.tables              WHERE table_schema = DATABASE() AND table_name = '_sqlx_migrations'",
        )
        .await
    }

    /// Connect with a bounded pool and read the server version.
    pub async fn connect(url: &str) -> Result<Self, RelError> {
        let pool = MySqlPoolOptions::new()
            .max_connections(16)
            .acquire_timeout(std::time::Duration::from_secs(10))
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

    /// Connect with an explicit TLS policy and pool bounds, to a connection string held as a
    /// [`persistant_relational::ConnectionSecret`]. The policy **overrides** the URL's `ssl-mode`,
    /// so a URL cannot weaken it; the default policy is [`TlsPolicy::Require`].
    pub async fn connect_with(
        secret: &persistant_relational::ConnectionSecret,
        options: &ConnectOptions,
    ) -> Result<Self, RelError> {
        let url = secret.resolve()?;
        let mut o = MySqlConnectOptions::from_str(url.expose())
            .map_err(|_| RelError::Backend("connection string is not valid".to_string()))?;
        o = match &options.tls {
            TlsPolicy::Disable => o.ssl_mode(MySqlSslMode::Disabled),
            TlsPolicy::Prefer => o.ssl_mode(MySqlSslMode::Preferred),
            TlsPolicy::Require => o.ssl_mode(MySqlSslMode::Required),
            TlsPolicy::VerifyCa { root_cert } => {
                let o = o.ssl_mode(MySqlSslMode::VerifyCa);
                match root_cert {
                    Some(p) => o.ssl_ca(p),
                    None => o,
                }
            }
            TlsPolicy::VerifyFull { root_cert } => {
                let o = o.ssl_mode(MySqlSslMode::VerifyIdentity);
                match root_cert {
                    Some(p) => o.ssl_ca(p),
                    None => o,
                }
            }
        };
        let pool = MySqlPoolOptions::new()
            .max_connections(options.max_connections)
            .acquire_timeout(options.acquire_timeout)
            .connect_with(o)
            .await
            .map_err(classify)?;
        Self::from_pool(pool).await
    }

    /// Whether the connection this backend uses is encrypted, as the **server** reports it
    /// (`Ssl_cipher` is non-empty), not as the client was asked.
    pub async fn connection_is_encrypted(&self) -> Result<bool, RelError> {
        let (_, cipher): (String, String) = sqlx::query_as("SHOW SESSION STATUS LIKE 'Ssl_cipher'")
            .fetch_one(&self.pool)
            .await
            .map_err(classify)?;
        Ok(!cipher.is_empty())
    }

    /// Wrap an existing pool. The dialect is `MariaDb` when the server says so, else `MySql`.
    pub async fn from_pool(pool: sqlx::MySqlPool) -> Result<Self, RelError> {
        let full: String = sqlx::query_scalar("SELECT VERSION()")
            .fetch_one(&pool)
            .await
            .map_err(classify)?;
        // "8.4.9" -> MySQL; "11.4.5-MariaDB-ubu2404" -> MariaDB, version "11.4.5".
        let dialect = if full.contains("MariaDB") {
            Dialect::MariaDb
        } else {
            Dialect::MySql
        };
        let version = full.split(['-', ' ']).next().unwrap_or("").to_string();
        Ok(MysqlBackend {
            pool,
            version,
            dialect,
        })
    }

    /// The concrete pool.
    pub fn pool(&self) -> &sqlx::MySqlPool {
        &self.pool
    }
}

impl RelationalBackend for MysqlBackend {
    type Tx<'a> = MysqlTx;

    fn backend(&self) -> &str {
        match self.dialect {
            Dialect::MariaDb => "mariadb",
            _ => "mysql",
        }
    }

    fn version(&self) -> &str {
        &self.version
    }

    fn dialect(&self) -> Dialect {
        self.dialect.clone()
    }

    async fn execute(&self, sql: &str, params: &[Value]) -> Result<u64, RelError> {
        if params.is_empty() {
            return text_execute(sql, &self.pool).await;
        }
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

    async fn begin(&self, isolation: Isolation) -> Result<MysqlTx, RelError> {
        let statement = match isolation {
            Isolation::ReadCommitted => {
                "SET TRANSACTION ISOLATION LEVEL READ COMMITTED; START TRANSACTION"
            }
            Isolation::RepeatableRead => {
                "SET TRANSACTION ISOLATION LEVEL REPEATABLE READ; START TRANSACTION"
            }
            Isolation::Serializable => {
                "SET TRANSACTION ISOLATION LEVEL SERIALIZABLE; START TRANSACTION"
            }
            other => return Err(RelError::Unsupported(format!("isolation level {other:?}"))),
        };
        Ok(MysqlTx {
            // The default is READ COMMITTED: under InnoDB's default REPEATABLE READ a locking read or an
            // `UPDATE` that matches no row takes a gap lock, and two transactions that then insert
            // into the same gap deadlock. The statement runs as a text query, so the isolation
            // level applies to exactly this transaction.
            tx: self.pool.begin_with(statement).await.map_err(classify)?,
        })
    }
}

/// Run a statement with no parameters over MySQL's text protocol. The prepared-statement protocol
/// refuses some statements outright (`SAVEPOINT`, error 1295), and a statement with nothing to bind
/// gains nothing from being prepared.
async fn text_execute<'e, E>(sql: &str, executor: E) -> Result<u64, RelError>
where
    E: sqlx::Executor<'e, Database = MySql>,
{
    sqlx::raw_sql(AssertSqlSafe(sql.to_string()))
        .execute(executor)
        .await
        .map(|r| r.rows_affected())
        .map_err(classify)
}

/// A MySQL transaction.
#[derive(Debug)]
pub struct MysqlTx {
    tx: sqlx::Transaction<'static, MySql>,
}

impl RelationalTx for MysqlTx {
    async fn execute(&mut self, sql: &str, params: &[Value]) -> Result<u64, RelError> {
        if params.is_empty() {
            return text_execute(sql, &mut *self.tx).await;
        }
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

impl persistant_relational::Migrates for MysqlBackend {
    async fn migrate(
        &self,
        migrations: &[persistant_relational::Migration],
    ) -> Result<persistant_relational::MigrateReport, persistant_relational::MigrateError> {
        use crate::migrate::run_on_own_connection;
        run_on_own_connection!(
            self.pool,
            migrations,
            classify,
            "SELECT COUNT(*) FROM information_schema.tables \
             WHERE table_schema = DATABASE() AND table_name = '_sqlx_migrations'",
            if self.dialect == Dialect::MariaDb {
                Some((
                    "SELECT GET_LOCK(CONCAT('persistant_migrate:', DATABASE()), 86400)",
                    "SELECT RELEASE_LOCK(CONCAT('persistant_migrate:', DATABASE()))",
                ))
            } else {
                None
            }
        )
    }

    async fn applied_versions(&self) -> Result<Vec<i64>, persistant_relational::MigrateError> {
        use crate::migrate::backend_error;
        let exists: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM information_schema.tables \
             WHERE table_schema = DATABASE() AND table_name = '_sqlx_migrations'",
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
