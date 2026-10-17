// SPDX-License-Identifier: MIT OR Apache-2.0
//! Checksummed, ordered, resumable schema migrations as a **capability**: the types a driver speaks and
//! the [`Migrates`] trait it implements. The driver crate owns the ledger (`persistant-sqlx` keeps sqlx's
//! own `_sqlx_migrations` table, so a database can be handled by this crate or by `sqlx migrate run`
//! without a conversion). What the ledger promises is decided by
//! [`migrate_conformance`](crate::migrate_conformance), not asserted here.

use crate::{RelError, RelationalBackend};
use std::fmt;
use std::future::Future;

/// One migration: a version, a description and the SQL text. The text is hashed; changing a
/// migration that has been applied is an error, never a silent re-run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Migration {
    /// Positive, strictly increasing across a program's migrations (sqlx uses the timestamp or
    /// sequence in the file name).
    pub version: i64,
    /// A short description, stored in the ledger.
    pub description: String,
    /// The statements, run in one transaction.
    pub sql: String,
}

impl Migration {
    /// A migration.
    pub fn new(version: i64, description: impl Into<String>, sql: impl Into<String>) -> Self {
        Migration {
            version,
            description: description.into(),
            sql: sql.into(),
        }
    }
}

/// Why a migration run stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum MigrateError {
    /// A migration already applied has different text now (its checksum differs).
    ChecksumMismatch {
        /// The version whose text changed.
        version: i64,
    },
    /// The database has applied a version this program does not know: it is older than the
    /// database it is pointed at. The program refuses rather than run against a newer schema.
    NewerThanProgram {
        /// The unknown applied version.
        version: i64,
    },
    /// A migration is recorded as partially applied (`success = false`); a person must fix it.
    Dirty {
        /// The version.
        version: i64,
    },
    /// A migration's SQL failed; its transaction was rolled back.
    Failed {
        /// The version.
        version: i64,
        /// The database's classification of the failure.
        error: RelError,
    },
    /// Anything else (connection, ledger access).
    Backend(String),
}

impl fmt::Display for MigrateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MigrateError::ChecksumMismatch { version } => {
                write!(f, "migration {version} was applied and has been modified")
            }
            MigrateError::NewerThanProgram { version } => write!(
                f,
                "the database has applied migration {version}, which this program does not know"
            ),
            MigrateError::Dirty { version } => {
                write!(
                    f,
                    "migration {version} is partially applied; fix it by hand"
                )
            }
            MigrateError::Failed { version, error } => {
                write!(f, "migration {version} failed: {error}")
            }
            MigrateError::Backend(m) => write!(f, "migration: {m}"),
        }
    }
}

impl std::error::Error for MigrateError {}

/// What a successful run did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrateReport {
    /// Versions applied by this run, ascending.
    pub applied: Vec<i64>,
    /// Versions that were already applied and still match.
    pub already_applied: Vec<i64>,
    /// The checksum the ledger holds for every version applied after this run (both lists above), as
    /// `(version, checksum)` ascending. A backend whose ledger keeps none leaves this empty.
    pub checksums: Vec<(i64, Vec<u8>)>,
}

/// A relational backend that keeps a migration ledger.
///
/// The ledger lives in the connection's current schema (or database): point this at a database a
/// program owns, never at one whose ledger belongs to something else.
pub trait Migrates: RelationalBackend {
    /// Apply `migrations` in version order, skipping those already applied. Each migration runs in
    /// its own transaction where the database can; refuses (applying nothing further) if an applied
    /// migration's text has changed, if the database has applied a version the program does not
    /// know, or if a migration is recorded as dirty.
    fn migrate(
        &self,
        migrations: &[Migration],
    ) -> impl Future<Output = Result<MigrateReport, MigrateError>> + Send;

    /// The versions the ledger says succeeded, ascending; empty if there is no ledger yet.
    fn applied_versions(&self) -> impl Future<Output = Result<Vec<i64>, MigrateError>> + Send;
}
