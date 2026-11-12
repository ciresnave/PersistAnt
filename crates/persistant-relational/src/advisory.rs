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
//!
//! **Which key.** [`lock`] / [`try_lock`] use `hashtext(name)` (32 bits, the original key). [`lock64`] /
//! [`try_lock64`] use `hashtextextended(name, 0)` (64 bits, PostgreSQL 11+): far fewer false collisions,
//! and the key a program that already locks `pg_advisory_xact_lock(hashtextextended('name', 0))` shares.
//! The two keys are different locks: a 32-bit holder and a 64-bit holder of one name do **not**
//! contend, so move every taker of a name to one variant together (a rolling restart that mixes them
//! has no mutual exclusion). Pick the variant the other takers (a trigger, another program) already use;
//! for a new lock with no other taker, prefer the 64-bit one.

use crate::{Dialect, RelError, RelationalTx, Value};

fn require_postgres(dialect: &Dialect) -> Result<(), RelError> {
    match dialect {
        Dialect::Postgres => Ok(()),
        other => Err(RelError::Unsupported(format!(
            "advisory locks are PostgreSQL's; {other} has no transaction-scoped named lock"
        ))),
    }
}

/// Which hash turns the name into the advisory key.
#[derive(Clone, Copy)]
enum Key {
    /// `hashtext(name)`, 32 bits.
    Hash32,
    /// `hashtextextended(name, 0)`, 64 bits.
    Hash64,
}

impl Key {
    fn lock_sql(self) -> &'static str {
        match self {
            Key::Hash32 => "SELECT pg_advisory_xact_lock(hashtext($1))",
            Key::Hash64 => "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))",
        }
    }
    fn try_sql(self) -> &'static str {
        match self {
            Key::Hash32 => "SELECT pg_try_advisory_xact_lock(hashtext($1))",
            Key::Hash64 => "SELECT pg_try_advisory_xact_lock(hashtextextended($1, 0))",
        }
    }
}

async fn lock_with<T: RelationalTx>(
    tx: &mut T,
    dialect: &Dialect,
    name: &str,
    key: Key,
) -> Result<(), RelError> {
    require_postgres(dialect)?;
    tx.execute(key.lock_sql(), &[Value::Text(name.to_string())])
        .await
        .map(|_| ())
}

async fn try_lock_with<T: RelationalTx>(
    tx: &mut T,
    dialect: &Dialect,
    name: &str,
    key: Key,
) -> Result<bool, RelError> {
    require_postgres(dialect)?;
    let rows = tx
        .query(key.try_sql(), &[Value::Text(name.to_string())])
        .await?;
    match rows.first().and_then(|r| r.values.first()) {
        Some(Value::Bool(got)) => Ok(*got),
        other => Err(RelError::Unsupported(format!(
            "pg_try_advisory_xact_lock returned {other:?}, not a boolean"
        ))),
    }
}

/// Take the lock `name` for the rest of `tx`, waiting for its current holder to finish. Taking a lock
/// the same transaction already holds returns at once. Key: `hashtext(name)` (32 bits).
pub async fn lock<T: RelationalTx>(
    tx: &mut T,
    dialect: &Dialect,
    name: &str,
) -> Result<(), RelError> {
    lock_with(tx, dialect, name, Key::Hash32).await
}

/// Take the lock `name` for the rest of `tx` if nobody else holds it. `Ok(false)`: another transaction
/// holds it, and nothing was taken. Key: `hashtext(name)` (32 bits).
pub async fn try_lock<T: RelationalTx>(
    tx: &mut T,
    dialect: &Dialect,
    name: &str,
) -> Result<bool, RelError> {
    try_lock_with(tx, dialect, name, Key::Hash32).await
}

/// As [`lock`], with the 64-bit key `hashtextextended(name, 0)` (PostgreSQL 11+). A different lock from
/// [`lock`]'s for the same name: see the module notes on which to use.
pub async fn lock64<T: RelationalTx>(
    tx: &mut T,
    dialect: &Dialect,
    name: &str,
) -> Result<(), RelError> {
    lock_with(tx, dialect, name, Key::Hash64).await
}

/// As [`try_lock`], with the 64-bit key `hashtextextended(name, 0)` (PostgreSQL 11+).
pub async fn try_lock64<T: RelationalTx>(
    tx: &mut T,
    dialect: &Dialect,
    name: &str,
) -> Result<bool, RelError> {
    try_lock_with(tx, dialect, name, Key::Hash64).await
}
