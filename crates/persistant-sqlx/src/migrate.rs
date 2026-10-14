// SPDX-License-Identifier: MIT OR Apache-2.0
//! Checksummed, ordered, resumable schema migrations that **keep sqlx's ledger**.
//!
//! The ledger is sqlx's own `_sqlx_migrations` table, written by sqlx's own `Migrator`, so its
//! shape and its checksums (SHA-384 of the migration text) are exactly what `sqlx::migrate!` and
//! `sqlx migrate run` write and read. That is a constraint, not a preference: Humboldt's transfer
//! tool compares versions *and* checksums from that table (`docs/DESIGN-KINDS.md` §5), and a
//! database can be handled by either this crate or sqlx's CLI without a conversion.
//!
//! What this layer adds is a typed outcome instead of an sqlx error string, and a report of what it
//! applied.

use crate::{PgBackend, classify};
use persistant_relational::RelError;
use sqlx::SqlSafeStr;
use sqlx::migrate::{Migration as SqlxMigration, MigrationType, Migrator};
use std::fmt;

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
}

impl PgBackend {
    /// Apply `migrations` in version order, skipping those already applied, in the ledger
    /// `_sqlx_migrations` of the connection's current schema.
    ///
    /// Each migration runs in its own transaction. Refuses (applying nothing further) if an applied
    /// migration's text has changed, if the database has applied a version the program does not
    /// know, or if a migration is recorded as dirty.
    pub async fn migrate(&self, migrations: &[Migration]) -> Result<MigrateReport, MigrateError> {
        let sqlx_migrations: Vec<SqlxMigration> = migrations
            .iter()
            .map(|m| {
                SqlxMigration::new(
                    m.version,
                    m.description.clone().into(),
                    MigrationType::Simple,
                    sqlx::AssertSqlSafe(m.sql.clone()).into_sql_str(),
                    false,
                )
            })
            .collect();
        let before = self.applied_versions().await?;
        let migrator = Migrator::with_migrations(sqlx_migrations);
        migrator.run(&self.pool).await.map_err(|e| match e {
            sqlx::migrate::MigrateError::VersionMismatch(version) => {
                MigrateError::ChecksumMismatch { version }
            }
            sqlx::migrate::MigrateError::VersionMissing(version) => {
                MigrateError::NewerThanProgram { version }
            }
            sqlx::migrate::MigrateError::Dirty(version) => MigrateError::Dirty { version },
            sqlx::migrate::MigrateError::ExecuteMigration(e, version) => MigrateError::Failed {
                version,
                error: classify(e),
            },
            other => MigrateError::Backend(other.to_string()),
        })?;
        let after = self.applied_versions().await?;
        let applied = after
            .iter()
            .copied()
            .filter(|v| !before.contains(v))
            .collect();
        Ok(MigrateReport {
            applied,
            already_applied: before,
        })
    }

    /// The versions the ledger says succeeded, ascending; empty if there is no ledger yet.
    pub async fn applied_versions(&self) -> Result<Vec<i64>, MigrateError> {
        let exists: bool = sqlx::query_scalar("SELECT to_regclass('_sqlx_migrations') IS NOT NULL")
            .fetch_one(&self.pool)
            .await
            .map_err(|e| MigrateError::Backend(e.to_string()))?;
        if !exists {
            return Ok(Vec::new());
        }
        sqlx::query_scalar("SELECT version FROM _sqlx_migrations WHERE success ORDER BY version")
            .fetch_all(&self.pool)
            .await
            .map_err(|e| MigrateError::Backend(e.to_string()))
    }
}
