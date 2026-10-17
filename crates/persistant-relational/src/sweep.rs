// SPDX-License-Identifier: MIT OR Apache-2.0
//! Expiry sweep: delete the rows of a consumer's own table whose timestamp column is older than a
//! cutoff, in bounded batches.
//!
//! **This is a mechanism, not a policy.** There is no default cutoff, no schedule and no table list:
//! which tables expire, by which column, and how long a row lives belong to the program that owns the
//! table (its retention policy). The key-value kind's own `kv.purge_expired` is separate.
//!
//! **Batches.** Each batch is one autocommit `DELETE` of at most `batch` rows, so a sweep over millions
//! of expired rows holds row locks for one batch at a time and never one long transaction. The sweep
//! repeats until a batch deletes nothing, or until `max_batches` batches have run
//! ([`SweepReport::complete`] is then `false` and a later call takes the rest).
//!
//! **Concurrent writers.** Only rows whose column is `< cutoff` *at the time the batch runs* are
//! deleted: a row whose timestamp another transaction moved past the cutoff (a refreshed session) is
//! kept, on every dialect. A sweep may delete fewer rows than exist under contention; the next call
//! takes the rest. Rows whose column is `NULL` are never deleted (SQL comparison with `NULL`).
//!
//! **Names.** `table` (optionally `schema.table`) and `column` are used as written, unquoted, and must
//! be plain identifiers (ASCII letters, digits, `_`, not starting with a digit): nothing else is
//! accepted, so a name can never carry SQL. A reserved word as a name is the caller's to avoid.
//!
//! **SQLite** deletes by `rowid`: a `WITHOUT ROWID` table is refused by the database.

use crate::{Dialect, RelError, RelationalBackend, Value};
use std::fmt;

/// What a sweep did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SweepReport {
    /// Rows deleted over all batches.
    pub deleted: u64,
    /// Batches run, including the final one that found nothing (when `complete`).
    pub batches: u32,
    /// `true` when a batch found nothing left to delete; `false` when `max_batches` stopped it first.
    pub complete: bool,
}

/// Why a sweep did not run or did not finish.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum SweepError {
    /// `table` or `column` is not a plain identifier.
    BadName(String),
    /// `batch` or `max_batches` is zero.
    ZeroBound,
    /// The cutoff is `NULL`, which would match no row.
    NullCutoff,
    /// The database refused a batch. Batches before it stay deleted.
    Rel(RelError),
    /// The backend speaks a dialect this function has no statement for.
    Dialect(String),
}

impl fmt::Display for SweepError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SweepError::BadName(n) => write!(f, "not a plain identifier: {n:?}"),
            SweepError::ZeroBound => f.write_str("batch and max_batches must be at least 1"),
            SweepError::NullCutoff => f.write_str("the cutoff must not be NULL"),
            SweepError::Rel(e) => write!(f, "sweep: {e}"),
            SweepError::Dialect(d) => write!(f, "sweep: no statement for dialect {d}"),
        }
    }
}

impl std::error::Error for SweepError {}

fn ident(s: &str) -> bool {
    !s.is_empty()
        && !s.starts_with(|c: char| c.is_ascii_digit())
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn table_name(s: &str) -> bool {
    match s.split_once('.') {
        Some((schema, table)) => ident(schema) && ident(table),
        None => ident(s),
    }
}

/// The one-batch statement for `dialect`, with the cutoff as its only parameter.
fn statement(dialect: &Dialect, table: &str, column: &str, batch: u32) -> Option<String> {
    Some(match dialect {
        Dialect::Postgres => format!(
            "DELETE FROM {table} WHERE ctid IN \
             (SELECT ctid FROM {table} WHERE {column} < $1 LIMIT {batch})"
        ),
        Dialect::Sqlite => format!(
            "DELETE FROM {table} WHERE rowid IN \
             (SELECT rowid FROM {table} WHERE {column} < ? LIMIT {batch})"
        ),
        Dialect::MySql | Dialect::MariaDb => {
            format!("DELETE FROM {table} WHERE {column} < ? LIMIT {batch}")
        }
        _ => return None,
    })
}

/// Delete rows of `table` whose `column` is less than `cutoff`, at most `batch` per statement, for at
/// most `max_batches` statements. Batches, concurrent writers, `NULL` and naming rules are in the header of this file (`sweep.rs`).
pub async fn sweep<B: RelationalBackend>(
    db: &B,
    table: &str,
    column: &str,
    cutoff: &Value,
    batch: u32,
    max_batches: u32,
) -> Result<SweepReport, SweepError> {
    if !table_name(table) {
        return Err(SweepError::BadName(table.to_string()));
    }
    if !ident(column) {
        return Err(SweepError::BadName(column.to_string()));
    }
    if batch == 0 || max_batches == 0 {
        return Err(SweepError::ZeroBound);
    }
    if *cutoff == Value::Null {
        return Err(SweepError::NullCutoff);
    }
    let dialect = db.dialect();
    let sql = statement(&dialect, table, column, batch)
        .ok_or_else(|| SweepError::Dialect(dialect.to_string()))?;
    let mut report = SweepReport {
        deleted: 0,
        batches: 0,
        complete: false,
    };
    while report.batches < max_batches {
        let n = db
            .execute(&sql, std::slice::from_ref(cutoff))
            .await
            .map_err(SweepError::Rel)?;
        report.batches += 1;
        report.deleted += n;
        if n == 0 {
            report.complete = true;
            break;
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_plain_identifiers_only() {
        for ok in ["sessions", "_t1", "public.sessions", "A_b"] {
            assert!(table_name(ok), "{ok}");
        }
        for bad in [
            "",
            "1t",
            "t; DROP TABLE t",
            "a.b.c",
            ".t",
            "t.",
            "a b",
            "t\"",
            "t-1",
            "é",
        ] {
            assert!(!table_name(bad), "{bad:?}");
        }
        assert!(!ident("a.b"));
    }

    #[test]
    fn each_dialect_has_a_statement_with_one_parameter() {
        for d in [
            Dialect::Postgres,
            Dialect::Sqlite,
            Dialect::MySql,
            Dialect::MariaDb,
        ] {
            let s = statement(&d, "t", "c", 7).unwrap();
            assert!(s.contains("LIMIT 7"), "{s}");
            assert_eq!(s.matches("c < ").count(), 1);
        }
        assert!(statement(&Dialect::Other("x".into()), "t", "c", 1).is_none());
    }
}
