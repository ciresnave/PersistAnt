// SPDX-License-Identifier: MIT OR Apache-2.0
//! Contract tests for the migration ledger ([`Migrates`]): what it promises, decided against a real
//! database. Each leaf produces a matrix row, **on** only when the controls held.
//!
//! - `rel.migrate.apply`: migrations are applied in version order, the ledger lists them, and a second
//!   run applies nothing and does not run them again.
//! - `rel.migrate.checksum`: changing the text of an applied migration is refused as
//!   [`MigrateError::ChecksumMismatch`], not silently re-run or ignored.
//! - `rel.migrate.newer`: a program that knows fewer migrations than the database has applied is
//!   refused as [`MigrateError::NewerThanProgram`].
//! - `rel.migrate.atomic`: a migration whose statements fail part-way leaves none of its row changes,
//!   keeps the migrations before it, and is reported as [`MigrateError::Failed`] with the database's own
//!   classification of the failure.
//! - `rel.migrate.ddl_atomic`: the same, for a table the failed migration created. MySQL and MariaDB
//!   commit DDL implicitly and cannot undo it, so this leaf is off there.
//! - `rel.migrate.concurrent`: two migrators started at once against the same database apply each
//!   migration exactly once between them and both finish cleanly (the second waits or finds the work
//!   done); neither fails, and no migration runs twice.
//!
//! **Where it runs.** The ledger belongs to the connection's schema or database, so these tests must be
//! pointed at a **disposable** one (a scratch schema, a scratch database, a fresh file): each leaf
//! drops the ledger and its own tables first and last.
//!
//! **Controls.** The known-bad control is a real one: [`Defect::NoMigrationLedger`] wraps the same
//! target in a migrator that runs every migration's statements each time with no ledger, no checksum
//! and no transaction. Every leaf must fail it. The positive control is weaker (as for the other
//! relational leaves): the leaf's own tables could be created on the target at all.

use crate::conformance::{Defect, Defective, Failure};
use crate::{
    Dialect, MigrateError, MigrateReport, Migrates, Migration, RelError, RelationalBackend, Value,
};
use persistant_core::{CapabilityId, Config, Controls, Reason, Row as MatrixRow, Verdict};
use std::collections::BTreeMap;
use std::future::Future;
use std::task::Poll;

/// A leaf this module can decide.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum MigrateLeaf {
    /// `rel.migrate.apply`
    Apply,
    /// `rel.migrate.checksum`
    Checksum,
    /// `rel.migrate.newer`
    Newer,
    /// `rel.migrate.atomic`
    Atomic,
    /// `rel.migrate.ddl_atomic`
    DdlAtomic,
    /// `rel.migrate.concurrent`
    Concurrent,
}

impl MigrateLeaf {
    /// Every leaf with a test here.
    pub const ALL: [MigrateLeaf; 6] = [
        MigrateLeaf::Apply,
        MigrateLeaf::Checksum,
        MigrateLeaf::Newer,
        MigrateLeaf::Atomic,
        MigrateLeaf::DdlAtomic,
        MigrateLeaf::Concurrent,
    ];

    /// The capability id.
    pub fn id(self) -> &'static str {
        match self {
            MigrateLeaf::Apply => "rel.migrate.apply",
            MigrateLeaf::Checksum => "rel.migrate.checksum",
            MigrateLeaf::Newer => "rel.migrate.newer",
            MigrateLeaf::Atomic => "rel.migrate.atomic",
            MigrateLeaf::DdlAtomic => "rel.migrate.ddl_atomic",
            MigrateLeaf::Concurrent => "rel.migrate.concurrent",
        }
    }
}

const T: &str = "persistant_mig_t";
const T2: &str = "persistant_mig_t2";

fn wrong<X>(msg: impl Into<String>) -> Result<X, Failure> {
    Err(Failure::Wrong(msg.into()))
}

fn mig(version: i64, sql: &str) -> Migration {
    Migration::new(version, format!("persistant migrate leaf {version}"), sql)
}

fn create_t() -> Migration {
    mig(
        1,
        &format!("CREATE TABLE {T} (id BIGINT PRIMARY KEY, n BIGINT NOT NULL)"),
    )
}

async fn reset<M: Migrates>(m: &M) -> Result<(), RelError> {
    for t in [T, T2, "_sqlx_migrations"] {
        m.execute(&format!("DROP TABLE IF EXISTS {t}"), &[]).await?;
    }
    Ok(())
}

async fn count<M: Migrates>(m: &M, table: &str) -> Result<i64, RelError> {
    let rows = m
        .query(&format!("SELECT COUNT(*) FROM {table}"), &[])
        .await?;
    Ok(rows.first().and_then(|r| r.int(0)).unwrap_or(-1))
}

fn migrated<X>(r: Result<X, MigrateError>, what: &str) -> Result<X, Failure> {
    r.or_else(|e| wrong(format!("{what}: {e}")))
}

/// Run one leaf's test against `m`. Writes only to tables named `persistant_mig_*` and to the ledger
/// `_sqlx_migrations`, which it drops first and last.
pub async fn run_leaf<M: Migrates>(leaf: MigrateLeaf, m: &M) -> Result<(), Failure> {
    if matches!(leaf, MigrateLeaf::DdlAtomic)
        && matches!(m.dialect(), Dialect::MySql | Dialect::MariaDb)
    {
        return Err(Failure::Backend(RelError::Unsupported(
            "DDL commits implicitly: a failed migration cannot undo the tables it created"
                .to_string(),
        )));
    }
    reset(m).await?;
    let result = match leaf {
        MigrateLeaf::Apply => apply(m).await,
        MigrateLeaf::Checksum => checksum(m).await,
        MigrateLeaf::Newer => newer(m).await,
        MigrateLeaf::Atomic => atomic(m).await,
        MigrateLeaf::DdlAtomic => ddl_atomic(m).await,
        MigrateLeaf::Concurrent => concurrent(m).await,
    };
    reset(m).await?;
    result
}

async fn apply<M: Migrates>(m: &M) -> Result<(), Failure> {
    let migs = [
        create_t(),
        mig(2, &format!("INSERT INTO {T} (id, n) VALUES (1, 1)")),
    ];
    let first = migrated(m.migrate(&migs).await, "the first run")?;
    if first.applied != [1, 2] || !first.already_applied.is_empty() {
        return wrong(format!(
            "the first run reported {first:?}, not [1, 2] applied"
        ));
    }
    if count(m, T).await? != 1 {
        return wrong("migration 2 did not leave its row");
    }
    let ledger = migrated(m.applied_versions().await, "reading the ledger")?;
    if ledger != [1, 2] {
        return wrong(format!("the ledger lists {ledger:?}, not [1, 2]"));
    }
    let again = migrated(m.migrate(&migs).await, "the second run")?;
    if !again.applied.is_empty() || again.already_applied != [1, 2] {
        return wrong(format!(
            "the second run reported {again:?}, not [1, 2] already applied"
        ));
    }
    if count(m, T).await? != 1 {
        return wrong("the second run ran a migration again");
    }
    Ok(())
}

async fn checksum<M: Migrates>(m: &M) -> Result<(), Failure> {
    let migs = [
        create_t(),
        mig(2, &format!("INSERT INTO {T} (id, n) VALUES (1, 1)")),
    ];
    migrated(m.migrate(&migs).await, "the first run")?;
    let edited = [
        create_t(),
        mig(2, &format!("INSERT INTO {T} (id, n) VALUES (1, 2)")),
    ];
    match m.migrate(&edited).await {
        Err(MigrateError::ChecksumMismatch { version: 2 }) => Ok(()),
        other => wrong(format!(
            "an applied migration was edited and the run answered {other:?}, not a checksum mismatch on 2"
        )),
    }
}

async fn newer<M: Migrates>(m: &M) -> Result<(), Failure> {
    let migs = [
        create_t(),
        mig(2, &format!("INSERT INTO {T} (id, n) VALUES (1, 1)")),
    ];
    migrated(m.migrate(&migs).await, "the first run")?;
    match m.migrate(&migs[..1]).await {
        Err(MigrateError::NewerThanProgram { version: 2 }) => Ok(()),
        other => wrong(format!(
            "a program that knows only migration 1 met a database at 2 and the run answered {other:?}"
        )),
    }
}

async fn atomic<M: Migrates>(m: &M) -> Result<(), Failure> {
    let migs = [
        create_t(),
        mig(
            2,
            &format!(
                "INSERT INTO {T} (id, n) VALUES (1, 1); INSERT INTO {T} (id, n) VALUES (1, 2)"
            ),
        ),
    ];
    match m.migrate(&migs).await {
        Err(MigrateError::Failed {
            version: 2,
            error: RelError::UniqueViolation { .. },
        }) => {}
        other => {
            return wrong(format!(
                "a migration whose second statement repeats a key answered {other:?}, not a classified failure of 2"
            ));
        }
    }
    if count(m, T).await? != 0 {
        return wrong("the failed migration left the rows of its first statement behind");
    }
    let ledger = migrated(m.applied_versions().await, "reading the ledger")?;
    if ledger != [1] {
        return wrong(format!(
            "after the failure the ledger lists {ledger:?}, not [1]"
        ));
    }
    Ok(())
}

async fn ddl_atomic<M: Migrates>(m: &M) -> Result<(), Failure> {
    let migs = [
        create_t(),
        mig(
            2,
            &format!(
                "CREATE TABLE {T2} (id BIGINT PRIMARY KEY); INSERT INTO {T} (id, n) VALUES (1, 1); INSERT INTO {T} (id, n) VALUES (1, 2)"
            ),
        ),
    ];
    if !matches!(
        m.migrate(&migs).await,
        Err(MigrateError::Failed { version: 2, .. })
    ) {
        return wrong("the failing migration was not reported as failed");
    }
    // Control: the table migration 1 made is there and can be counted; a table that is not there cannot.
    count(m, T).await?;
    if count(m, T2).await.is_ok() {
        return wrong("the failed migration left the table it created behind");
    }
    Ok(())
}

/// Poll two futures together on this task until both are done.
async fn join2<A: Future, B: Future>(a: A, b: B) -> (A::Output, B::Output) {
    let mut a = std::pin::pin!(a);
    let mut b = std::pin::pin!(b);
    let (mut ra, mut rb) = (None, None);
    std::future::poll_fn(|cx| {
        if ra.is_none()
            && let Poll::Ready(v) = a.as_mut().poll(cx)
        {
            ra = Some(v);
        }
        if rb.is_none()
            && let Poll::Ready(v) = b.as_mut().poll(cx)
        {
            rb = Some(v);
        }
        if ra.is_some() && rb.is_some() {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    })
    .await;
    (ra.expect("a is done"), rb.expect("b is done"))
}

async fn concurrent<M: Migrates>(m: &M) -> Result<(), Failure> {
    let migs = [
        create_t(),
        mig(2, &format!("INSERT INTO {T} (id, n) VALUES (1, 1)")),
    ];
    let (one, two) = join2(m.migrate(&migs), m.migrate(&migs)).await;
    let one = migrated(one, "the first of two simultaneous runs")?;
    let two = migrated(two, "the second of two simultaneous runs")?;
    let mut applied: Vec<i64> = one.applied.iter().chain(&two.applied).copied().collect();
    applied.sort_unstable();
    if applied != [1, 2] {
        return wrong(format!(
            "two simultaneous runs applied {applied:?} between them, not each of [1, 2] once ({one:?}, {two:?})"
        ));
    }
    if count(m, T).await? != 1 {
        return wrong("a migration ran twice: its row is not there exactly once");
    }
    let ledger = migrated(m.applied_versions().await, "reading the ledger")?;
    if ledger != [1, 2] {
        return wrong(format!("the ledger lists {ledger:?}, not [1, 2]"));
    }
    Ok(())
}

/// The outcome of certifying one migration leaf.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationCertified {
    /// The leaf.
    pub leaf: MigrateLeaf,
    /// The row to record.
    pub row: MatrixRow,
}

/// Run every migration leaf against `target` (a **disposable** ledger, see the module docs) and return
/// one row per leaf. A leaf is **on** only if the target passed *and* the known-bad control failed.
pub async fn certify<M: Migrates>(
    target: &M,
    config: &Config,
    metadata: &Config,
) -> Vec<MigrationCertified> {
    let mut out = Vec::new();
    for leaf in MigrateLeaf::ALL {
        let result = run_leaf(leaf, target).await;
        let positive_passed = !matches!(
            &result,
            Err(Failure::Backend(
                RelError::Unsupported(_) | RelError::Unavailable(_)
            ))
        );
        let bad = Defective::new(target, Defect::NoMigrationLedger);
        let known_bad_failed = run_leaf(leaf, &bad).await.is_err();
        let controls = Controls {
            positive_passed,
            known_bad_failed,
        };
        let id = CapabilityId::new(leaf.id()).expect("leaf id is valid");
        let md = metadata.clone().with("test", format!("rel::{}", leaf.id()));
        let off = |reason: Reason, note: String| MatrixRow {
            subject_kind: "capability".to_string(),
            subject: id.clone(),
            backend: target.backend().to_string(),
            version: target.version().to_string(),
            config: config.clone(),
            enabled: false,
            reason: Some(reason),
            metadata: md.clone(),
            note: note.replace(['\t', '\n'], " "),
            extra: BTreeMap::new(),
        };
        let row = match (
            &result,
            controls.positive_passed && controls.known_bad_failed,
        ) {
            (Ok(()), true) => Verdict::Exact
                .into_row(
                    id.clone(),
                    target.backend(),
                    target.version(),
                    config.clone(),
                    md.clone(),
                    controls,
                )
                .expect("both controls hold"),
            (Ok(()), false) => off(
                Reason::Untested,
                format!("controls did not hold: {controls:?}"),
            ),
            (Err(Failure::Wrong(m)), _) => off(Reason::Wrong, m.clone()),
            (Err(Failure::Backend(e)), _) => off(Reason::Missing, e.to_string()),
        };
        out.push(MigrationCertified { leaf, row });
    }
    out
}

/// The known-bad migrator: no ledger, no checksum, no transaction. It runs each migration's
/// statements (split on `;`) straight on the wrapped backend every time it is asked.
impl<R: RelationalBackend> Migrates for Defective<'_, R> {
    async fn migrate(&self, migrations: &[Migration]) -> Result<MigrateReport, MigrateError> {
        if self.defect() != Defect::NoMigrationLedger {
            return Err(MigrateError::Backend(
                "this defect does not wrap a migrator".to_string(),
            ));
        }
        let mut applied = Vec::new();
        for m in migrations {
            for stmt in m.sql.split(';').map(str::trim).filter(|s| !s.is_empty()) {
                self.inner()
                    .execute(stmt, &[] as &[Value])
                    .await
                    .map_err(|error| MigrateError::Failed {
                        version: m.version,
                        error,
                    })?;
            }
            applied.push(m.version);
        }
        Ok(MigrateReport {
            applied,
            already_applied: Vec::new(),
            checksums: Vec::new(),
        })
    }

    async fn applied_versions(&self) -> Result<Vec<i64>, MigrateError> {
        Ok(Vec::new())
    }
}
