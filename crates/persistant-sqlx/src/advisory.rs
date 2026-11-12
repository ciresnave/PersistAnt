// SPDX-License-Identifier: MIT OR Apache-2.0
//! Named transaction locks on an sqlx connection the program already holds, for the module that is
//! moving to persistant one statement at a time and still has an `sqlx::Transaction` open: its
//! connection (`&mut *tx`) takes the lock, and the lock ends with that transaction. The same keys as
//! `persistant_relational::advisory` (read its notes on which variant to use): [`lock`] and
//! [`try_lock`] are `hashtext(name)` (32 bits), [`lock64`] and [`try_lock64`] are
//! `hashtextextended(name, 0)` (64 bits, PostgreSQL 11+), and a 32-bit and a 64-bit holder of one name
//! do **not** contend.
//!
//! Call these **inside a transaction**: outside one, PostgreSQL releases a transaction-scoped lock at
//! the end of the statement that took it, so nothing is held.

use crate::classify;
use persistant_relational::RelError;
use sqlx::PgConnection;

async fn take(conn: &mut PgConnection, sql: &'static str, name: &str) -> Result<(), RelError> {
    sqlx::query(sql)
        .bind(name)
        .execute(conn)
        .await
        .map(|_| ())
        .map_err(classify)
}

async fn try_take(
    conn: &mut PgConnection,
    sql: &'static str,
    name: &str,
) -> Result<bool, RelError> {
    sqlx::query_scalar(sql)
        .bind(name)
        .fetch_one(conn)
        .await
        .map_err(classify)
}

/// Take `name` for the rest of the transaction `conn` is in, waiting for its holder. Key: `hashtext`.
pub async fn lock(conn: &mut PgConnection, name: &str) -> Result<(), RelError> {
    take(conn, "SELECT pg_advisory_xact_lock(hashtext($1))", name).await
}

/// Take `name` if nobody holds it; `Ok(false)` if another transaction does. Key: `hashtext`.
pub async fn try_lock(conn: &mut PgConnection, name: &str) -> Result<bool, RelError> {
    try_take(conn, "SELECT pg_try_advisory_xact_lock(hashtext($1))", name).await
}

/// As [`lock`], with the 64-bit key `hashtextextended(name, 0)`.
pub async fn lock64(conn: &mut PgConnection, name: &str) -> Result<(), RelError> {
    take(
        conn,
        "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))",
        name,
    )
    .await
}

/// As [`try_lock`], with the 64-bit key `hashtextextended(name, 0)`.
pub async fn try_lock64(conn: &mut PgConnection, name: &str) -> Result<bool, RelError> {
    try_take(
        conn,
        "SELECT pg_try_advisory_xact_lock(hashtextextended($1, 0))",
        name,
    )
    .await
}
