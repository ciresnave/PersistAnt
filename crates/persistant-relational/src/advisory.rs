// SPDX-License-Identifier: MIT OR Apache-2.0
//! `rel.advisory_lock`: a named lock held by a transaction, released when it ends.
//!
//! A program that serialises work by name (Humboldt's six sites and one trigger) takes the lock
//! inside the transaction that does the work: the second taker waits (or, with [`try_lock`], is
//! refused) until the first commits or rolls back. Nothing is released by hand and a crashed
//! connection releases it.
//!
//! **PostgreSQL only.** The lock is `pg_advisory_xact_lock(hashtext(name))`, so a trigger or another
//! program that takes `pg_advisory_xact_lock(hashtext('the same name'))` contends with this one: the
//! name, not an integer, is the contract. Every other dialect returns [`RelError::Unsupported`]:
//! SQLite has one writer and no named locks, and MySQL's `GET_LOCK` belongs to the connection, not the
//! transaction, so it would outlive the work it guards. The leaf is `rel.advisory_lock`; it is on
//! exactly where its test passes.
//!
//! Two different names can hash to one lock (`hashtext` is 32 bits). That only ever makes work wait
//! that need not; it never lets two holders of one name in.

use crate::{Dialect, RelError, RelationalTx, Value};

fn require_postgres(dialect: &Dialect) -> Result<(), RelError> {
    match dialect {
        Dialect::Postgres => Ok(()),
        other => Err(RelError::Unsupported(format!(
            "advisory locks are PostgreSQL's; {other} has no transaction-scoped named lock"
        ))),
    }
}

/// Take the lock `name` for the rest of `tx`, waiting for its current holder to finish. Taking a lock
/// the same transaction already holds returns at once.
pub async fn lock<T: RelationalTx>(
    tx: &mut T,
    dialect: &Dialect,
    name: &str,
) -> Result<(), RelError> {
    require_postgres(dialect)?;
    tx.execute(
        "SELECT pg_advisory_xact_lock(hashtext($1))",
        &[Value::Text(name.to_string())],
    )
    .await
    .map(|_| ())
}

/// Take the lock `name` for the rest of `tx` if nobody else holds it. `Ok(false)`: another transaction
/// holds it, and nothing was taken.
pub async fn try_lock<T: RelationalTx>(
    tx: &mut T,
    dialect: &Dialect,
    name: &str,
) -> Result<bool, RelError> {
    require_postgres(dialect)?;
    let rows = tx
        .query(
            "SELECT pg_try_advisory_xact_lock(hashtext($1))",
            &[Value::Text(name.to_string())],
        )
        .await?;
    match rows.first().and_then(|r| r.values.first()) {
        Some(Value::Bool(got)) => Ok(*got),
        other => Err(RelError::Unsupported(format!(
            "pg_try_advisory_xact_lock returned {other:?}, not a boolean"
        ))),
    }
}
