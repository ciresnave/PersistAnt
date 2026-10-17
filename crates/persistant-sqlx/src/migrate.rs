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
//! applied. The types and the [`Migrates`] trait live in `persistant-relational`; this module
//! implements it for each backend, and `persistant_relational::migrate_conformance` decides what the
//! implementation actually promises.

use crate::{PgBackend, classify};
use persistant_relational::{MigrateError, MigrateReport, Migrates, Migration, RelError};
use sqlx::SqlSafeStr;
use sqlx::migrate::{Migration as SqlxMigration, MigrationType, Migrator};

/// The program's migrations as sqlx's own `Migrator`.
pub(crate) fn migrator(migrations: &[Migration]) -> Migrator {
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
    Migrator::with_migrations(sqlx_migrations)
}

/// sqlx's migration error as a typed one; `classify` is the backend's own error classification.
pub(crate) fn map_error(
    e: sqlx::migrate::MigrateError,
    classify: fn(sqlx::Error) -> RelError,
) -> MigrateError {
    match e {
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
    }
}

pub(crate) fn backend_error(e: sqlx::Error) -> MigrateError {
    MigrateError::Backend(e.to_string())
}

/// Run the migrations on one connection taken from the pool. sqlx holds a session advisory lock for the
/// length of a run and does **not** release it when a migration fails, so the pooled connection would keep
/// it and the next run, on another connection, would wait for it forever (measured on MySQL 8.4: a hang
/// after a failed migration). The lock belongs to the session, so after a failure the session is ended
/// instead of returned to the pool.
macro_rules! run_on_own_connection {
    ($pool:expr, $migrations:expr, $classify:expr) => {{
        let mut conn = $pool
            .acquire()
            .await
            .map_err(crate::migrate::backend_error)?;
        // `run_direct` is sqlx own way round "implementation of `Acquire` is not general enough".
        match crate::migrate::migrator($migrations)
            .run_direct(None, &mut *conn, false)
            .await
        {
            Ok(()) => Ok(()),
            Err(e) => {
                let _ = sqlx::Connection::close(conn.detach()).await;
                Err(crate::migrate::map_error(e, $classify))
            }
        }
    }};
}
pub(crate) use run_on_own_connection;

impl Migrates for PgBackend {
    async fn migrate(&self, migrations: &[Migration]) -> Result<MigrateReport, MigrateError> {
        let before = Migrates::applied_versions(self).await?;
        run_on_own_connection!(self.pool, migrations, classify)?;
        let after = Migrates::applied_versions(self).await?;
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

    async fn applied_versions(&self) -> Result<Vec<i64>, MigrateError> {
        let exists: bool = sqlx::query_scalar("SELECT to_regclass('_sqlx_migrations') IS NOT NULL")
            .fetch_one(&self.pool)
            .await
            .map_err(backend_error)?;
        if !exists {
            return Ok(Vec::new());
        }
        sqlx::query_scalar("SELECT version FROM _sqlx_migrations WHERE success ORDER BY version")
            .fetch_all(&self.pool)
            .await
            .map_err(backend_error)
    }
}

impl PgBackend {
    /// Apply `migrations` in version order, skipping those already applied, in the ledger
    /// `_sqlx_migrations` of the connection's current schema. See [`Migrates::migrate`].
    pub async fn migrate(&self, migrations: &[Migration]) -> Result<MigrateReport, MigrateError> {
        Migrates::migrate(self, migrations).await
    }

    /// The versions the ledger says succeeded, ascending; empty if there is no ledger yet.
    pub async fn applied_versions(&self) -> Result<Vec<i64>, MigrateError> {
        Migrates::applied_versions(self).await
    }
}
