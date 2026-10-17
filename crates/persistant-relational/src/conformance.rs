// SPDX-License-Identifier: MIT OR Apache-2.0
//! Contract tests for the relational kind: what [`RelationalBackend`] promises, decided against a real
//! database. Each leaf produces a matrix row, **on** only when the controls held.
//!
//! - `rel.values`: integers, text, bytes and floats written as parameters come back as written, and a
//!   `NULL` comes back as `Value::Null`.
//! - `rel.transaction`: a committed write is visible, a rolled-back one is not, and a transaction
//!   dropped without `commit` rolls back.
//! - `rel.error.unique`: a duplicate key is reported as [`RelError::UniqueViolation`], not as an
//!   opaque backend error.
//! - `rel.foreign_key`: a reference to a missing row is rejected as [`RelError::ForeignKeyViolation`].
//! - `rel.check_constraint`: a row violating a `CHECK` is rejected as [`RelError::CheckViolation`].
//! - `rel.isolation.no_dirty_read`: a write inside an open transaction is invisible to another
//!   connection until it commits, and visible after (read committed or stronger).
//! - `rel.isolation.repeatable_read`: inside one transaction begun at that level a row read twice reads the same,
//!   and a table counted twice counts the same, though another connection committed an update and an insert in
//!   between. SQLite is off: its `begin` is `BEGIN IMMEDIATE`, which holds the write lock, so another connection
//!   cannot commit in between and the probe cannot run.
//! - `rel.isolation.serializable`: PostgreSQL: two concurrent serializable transactions that each read both rows and
//!   then change a different one (write skew) cannot both commit, while at read committed they can (the control).
//!   MySQL and MariaDB: a plain read inside a serializable transaction takes a shared lock, so another connection's
//!   `FOR UPDATE NOWAIT` on that row is refused, and is not at read committed or after the transaction ends.
//!   SQLite is off for the same reason as above.
//! - `rel.row_lock`: a row locked `FOR UPDATE` by an open transaction cannot be locked by another connection
//!   (probed with `SKIP LOCKED`, which skips it) until the transaction ends. SQLite has no row locks, so it is off there.
//! - `rel.upsert_where`: `INSERT .. ON CONFLICT (..) DO UPDATE .. WHERE ..` updates only when the condition holds.
//!   MySQL and MariaDB have no such clause, so it is off there.
//! - `rel.savepoint`: inside a transaction, `ROLLBACK TO` a savepoint undoes the work after it and keeps the work before it.
//!
//! **Controls.** There is no reference relational engine to use as a positive control, so the positive
//! control is weaker than in the kv and blob suites: it is that the leaf's schema could be created on
//! the target at all (a failure to create it is "missing", not "wrong"). The **known-bad** controls are
//! real: a [`Defect`] wrapper that makes the same target misbehave in one way (no transactions, lossy
//! bytes, unclassified errors), and, for the two constraint leaves, the same test run against a schema
//! created *without* the constraint, which must be caught accepting the bad row.
//!
//! Statements are written per dialect (this is a probe of the dialect itself, not of translation).
//! Booleans are left out of `rel.values`: how a driver returns one (`BOOL`, `TINYINT(1)`, `0/1`) is a
//! documented difference, not a contract.

use crate::{Dialect, Isolation, RelError, RelationalBackend, RelationalTx, Row, Value};
use persistant_core::{CapabilityId, Config, Controls, Reason, Row as MatrixRow, Verdict};
use std::collections::BTreeMap;

/// A leaf this module can decide.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Leaf {
    /// `rel.values`
    Values,
    /// `rel.transaction`
    Transaction,
    /// `rel.error.unique`
    UniqueClassified,
    /// `rel.foreign_key`
    ForeignKey,
    /// `rel.check_constraint`
    CheckConstraint,
    /// `rel.isolation.no_dirty_read`
    IsolationNoDirtyRead,
    /// `rel.isolation.repeatable_read`
    IsolationRepeatableRead,
    /// `rel.isolation.serializable`
    IsolationSerializable,
    /// `rel.savepoint`
    Savepoint,
    /// `rel.row_lock`
    RowLock,
    /// `rel.upsert_where`
    UpsertWhere,
}

impl Leaf {
    /// Every leaf with a test here.
    pub const ALL: [Leaf; 11] = [
        Leaf::Values,
        Leaf::Transaction,
        Leaf::UniqueClassified,
        Leaf::ForeignKey,
        Leaf::CheckConstraint,
        Leaf::IsolationNoDirtyRead,
        Leaf::IsolationRepeatableRead,
        Leaf::IsolationSerializable,
        Leaf::Savepoint,
        Leaf::RowLock,
        Leaf::UpsertWhere,
    ];

    /// The capability id.
    pub fn id(self) -> &'static str {
        match self {
            Leaf::Values => "rel.values",
            Leaf::Transaction => "rel.transaction",
            Leaf::UniqueClassified => "rel.error.unique",
            Leaf::ForeignKey => "rel.foreign_key",
            Leaf::CheckConstraint => "rel.check_constraint",
            Leaf::IsolationNoDirtyRead => "rel.isolation.no_dirty_read",
            Leaf::IsolationRepeatableRead => "rel.isolation.repeatable_read",
            Leaf::IsolationSerializable => "rel.isolation.serializable",
            Leaf::Savepoint => "rel.savepoint",
            Leaf::RowLock => "rel.row_lock",
            Leaf::UpsertWhere => "rel.upsert_where",
        }
    }

    fn defects(self) -> &'static [Defect] {
        match self {
            Leaf::Values => &[Defect::LossyBytes],
            Leaf::Transaction
            | Leaf::IsolationNoDirtyRead
            | Leaf::IsolationRepeatableRead
            | Leaf::IsolationSerializable
            | Leaf::Savepoint
            | Leaf::RowLock => &[Defect::NoTransactions],
            Leaf::UniqueClassified => &[Defect::UnclassifiedErrors],
            Leaf::ForeignKey | Leaf::CheckConstraint | Leaf::UpsertWhere => &[],
        }
    }

    /// Whether the leaf has a "schema without the constraint" known-bad run.
    fn has_unconstrained_control(self) -> bool {
        matches!(
            self,
            Leaf::ForeignKey | Leaf::CheckConstraint | Leaf::UpsertWhere
        )
    }
}

/// One way for a wrapped backend to be wrong, for the known-bad controls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Defect {
    /// `begin` returns a transaction whose statements run at once and whose `rollback` and drop do
    /// nothing.
    NoTransactions,
    /// A byte parameter is cut to its first byte.
    LossyBytes,
    /// Every error comes back as `RelError::Backend`.
    UnclassifiedErrors,
}

/// A backend with one [`Defect`] injected. The wrapped backend does the work.
#[derive(Debug)]
pub struct Defective<'a, R> {
    inner: &'a R,
    defect: Defect,
}

impl<'a, R: RelationalBackend> Defective<'a, R> {
    /// Wrap `inner` so it is wrong in exactly the way `defect` says.
    pub fn new(inner: &'a R, defect: Defect) -> Self {
        Defective { inner, defect }
    }
}

/// The transaction of a [`Defective`] backend.
pub enum DefectiveTx<'a, R: RelationalBackend + 'a> {
    /// The real transaction (the defect is elsewhere).
    Real(R::Tx<'a>, Defect),
    /// No transaction at all: statements run on the backend directly.
    Direct(&'a R),
}

impl<R: RelationalBackend> RelationalBackend for Defective<'_, R> {
    type Tx<'a>
        = DefectiveTx<'a, R>
    where
        Self: 'a;

    fn backend(&self) -> &str {
        self.inner.backend()
    }

    fn version(&self) -> &str {
        self.inner.version()
    }

    fn dialect(&self) -> Dialect {
        self.inner.dialect()
    }

    async fn execute(&self, sql: &str, params: &[Value]) -> Result<u64, RelError> {
        self.inner
            .execute(sql, &lossy(self.defect, params))
            .await
            .map_err(|e| unclassify(self.defect, e))
    }

    async fn query(&self, sql: &str, params: &[Value]) -> Result<Vec<Row>, RelError> {
        self.inner
            .query(sql, &lossy(self.defect, params))
            .await
            .map_err(|e| unclassify(self.defect, e))
    }

    async fn begin(&self, isolation: Isolation) -> Result<DefectiveTx<'_, R>, RelError> {
        if self.defect == Defect::NoTransactions {
            return Ok(DefectiveTx::Direct(self.inner));
        }
        Ok(DefectiveTx::Real(
            self.inner
                .begin(isolation)
                .await
                .map_err(|e| unclassify(self.defect, e))?,
            self.defect,
        ))
    }
}

fn lossy(defect: Defect, params: &[Value]) -> Vec<Value> {
    params
        .iter()
        .map(|p| match (defect, p) {
            (Defect::LossyBytes, Value::Bytes(b)) => {
                Value::Bytes(b.iter().take(1).copied().collect())
            }
            _ => p.clone(),
        })
        .collect()
}

fn unclassify(defect: Defect, e: RelError) -> RelError {
    if defect == Defect::UnclassifiedErrors {
        RelError::Backend(e.to_string())
    } else {
        e
    }
}

impl<R: RelationalBackend> RelationalTx for DefectiveTx<'_, R> {
    async fn execute(&mut self, sql: &str, params: &[Value]) -> Result<u64, RelError> {
        match self {
            DefectiveTx::Real(tx, d) => tx
                .execute(sql, &lossy(*d, params))
                .await
                .map_err(|e| unclassify(*d, e)),
            // With no transaction there is nothing for a savepoint to be in: the statements are
            // no-ops (run on a pool they would open a transaction on one connection and leak it).
            DefectiveTx::Direct(_)
                if sql.starts_with("SAVEPOINT")
                    || sql.starts_with("RELEASE")
                    || sql.starts_with("ROLLBACK TO") =>
            {
                Ok(0)
            }
            DefectiveTx::Direct(db) => db.execute(sql, params).await,
        }
    }

    async fn query(&mut self, sql: &str, params: &[Value]) -> Result<Vec<Row>, RelError> {
        match self {
            DefectiveTx::Real(tx, d) => tx
                .query(sql, &lossy(*d, params))
                .await
                .map_err(|e| unclassify(*d, e)),
            DefectiveTx::Direct(db) => db.query(sql, params).await,
        }
    }

    async fn commit(self) -> Result<(), RelError> {
        match self {
            DefectiveTx::Real(tx, d) => tx.commit().await.map_err(|e| unclassify(d, e)),
            DefectiveTx::Direct(_) => Ok(()),
        }
    }

    async fn rollback(self) -> Result<(), RelError> {
        match self {
            DefectiveTx::Real(tx, d) => tx.rollback().await.map_err(|e| unclassify(d, e)),
            DefectiveTx::Direct(_) => Ok(()),
        }
    }
}

/// Why a leaf's test did not pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// The backend returned something other than the contract says.
    Wrong(String),
    /// The backend could not do something the test needs (including creating the leaf's schema).
    Backend(RelError),
}

impl From<RelError> for Failure {
    fn from(e: RelError) -> Self {
        Failure::Backend(e)
    }
}

fn wrong<T>(msg: impl Into<String>) -> Result<T, Failure> {
    Err(Failure::Wrong(msg.into()))
}

// ---- per-dialect statement text -------------------------------------------------------------

#[derive(Clone, Copy)]
pub(crate) enum Ty {
    Int,
    Text,
    Bytes,
    Float,
}

pub(crate) struct Sql(pub(crate) Dialect);

impl Sql {
    /// The placeholder for parameter `n` (1-based).
    pub(crate) fn p(&self, n: usize) -> String {
        match self.0 {
            Dialect::Postgres => format!("${n}"),
            Dialect::Sqlite => format!("?{n}"),
            _ => "?".to_string(),
        }
    }

    pub(crate) fn ty(&self, t: Ty) -> &'static str {
        match (&self.0, t) {
            (_, Ty::Int) => "BIGINT",
            (Dialect::Postgres, Ty::Text) => "TEXT",
            (Dialect::Sqlite, Ty::Text) => "TEXT",
            (_, Ty::Text) => "VARCHAR(255)",
            (Dialect::Postgres, Ty::Bytes) => "BYTEA",
            (Dialect::Sqlite, Ty::Bytes) => "BLOB",
            (_, Ty::Bytes) => "LONGBLOB",
            (Dialect::Postgres, Ty::Float) => "DOUBLE PRECISION",
            (_, Ty::Float) => "DOUBLE",
        }
    }

    /// Table options (InnoDB is explicit on MySQL so constraints and transactions exist).
    pub(crate) fn tail(&self) -> &'static str {
        match self.0 {
            Dialect::MySql | Dialect::MariaDb => " ENGINE=InnoDB",
            _ => "",
        }
    }

    /// An integer primary key column definition.
    pub(crate) fn pk(&self, name: &str) -> String {
        format!("{name} BIGINT NOT NULL PRIMARY KEY")
    }
}

fn supported(d: &Dialect) -> Result<(), Failure> {
    match d {
        Dialect::Postgres | Dialect::Sqlite | Dialect::MySql | Dialect::MariaDb => Ok(()),
        other => Err(Failure::Backend(RelError::Unsupported(format!(
            "the relational contract tests have no statements for {other}"
        )))),
    }
}

pub(crate) async fn drop_tables<R: RelationalBackend>(
    b: &R,
    names: &[&str],
) -> Result<(), RelError> {
    for n in names {
        b.execute(&format!("DROP TABLE IF EXISTS {n}"), &[]).await?;
    }
    Ok(())
}

/// Run one leaf's test against `b`. `constrained = false` creates the constraint leaves' schema without
/// the constraint (the known-bad control); it makes no difference to the other leaves. Writes only to
/// tables named `persistant_rel_*`, which it drops first and last.
pub async fn run_leaf<R: RelationalBackend>(
    leaf: Leaf,
    b: &R,
    constrained: bool,
) -> Result<(), Failure> {
    let d = b.dialect();
    supported(&d)?;
    let s = Sql(d);
    match leaf {
        Leaf::Values => values(b, &s).await,
        Leaf::Transaction => transaction(b, &s).await,
        Leaf::UniqueClassified => unique(b, &s).await,
        Leaf::ForeignKey => foreign_key(b, &s, constrained).await,
        Leaf::CheckConstraint => check(b, &s, constrained).await,
        Leaf::IsolationNoDirtyRead => no_dirty_read(b, &s).await,
        Leaf::IsolationRepeatableRead => repeatable_read(b, &s).await,
        Leaf::IsolationSerializable => serializable(b, &s).await,
        Leaf::Savepoint => savepoint(b, &s).await,
        Leaf::RowLock => row_lock(b, &s).await,
        Leaf::UpsertWhere => upsert_where(b, &s, constrained).await,
    }
}

async fn values<R: RelationalBackend>(b: &R, s: &Sql) -> Result<(), Failure> {
    const T: &str = "persistant_rel_values";
    drop_tables(b, &[T]).await?;
    b.execute(
        &format!(
            "CREATE TABLE {T} ({}, i {}, t {}, b {}, f {}){}",
            s.pk("id"),
            s.ty(Ty::Int),
            s.ty(Ty::Text),
            s.ty(Ty::Bytes),
            s.ty(Ty::Float),
            s.tail()
        ),
        &[],
    )
    .await?;
    let cases: Vec<(i64, Value, Value, Value, Value)> = vec![
        (
            1,
            Value::Int(i64::MIN),
            Value::Text("ünï ✓ \u{1F980}".to_string()),
            Value::Bytes(vec![0x00, 0xFF, 0x80, 0x00, 0x7F]),
            Value::Float(1.5),
        ),
        (
            2,
            Value::Int(i64::MAX),
            Value::Text(String::new()),
            Value::Bytes(vec![]),
            Value::Float(-0.25),
        ),
        (3, Value::Null, Value::Null, Value::Null, Value::Null),
    ];
    for (id, i, t, bytes, f) in &cases {
        b.execute(
            &format!(
                "INSERT INTO {T} (id, i, t, b, f) VALUES ({}, {}, {}, {}, {})",
                s.p(1),
                s.p(2),
                s.p(3),
                s.p(4),
                s.p(5)
            ),
            &[
                Value::Int(*id),
                i.clone(),
                t.clone(),
                bytes.clone(),
                f.clone(),
            ],
        )
        .await?;
    }
    let rows = b
        .query(&format!("SELECT id, i, t, b, f FROM {T} ORDER BY id"), &[])
        .await?;
    drop_tables(b, &[T]).await?;
    if rows.len() != cases.len() {
        return wrong(format!("wrote {} rows, read {}", cases.len(), rows.len()));
    }
    for (row, (id, i, t, bytes, f)) in rows.iter().zip(&cases) {
        let want = [
            Value::Int(*id),
            i.clone(),
            t.clone(),
            bytes.clone(),
            f.clone(),
        ];
        // An empty byte string and an empty text may come back as NULL on no engine here; they must not.
        if row.values != want {
            return wrong(format!("row {id}: wrote {want:?}, read {:?}", row.values));
        }
    }
    Ok(())
}

async fn count<R: RelationalBackend>(b: &R, table: &str) -> Result<i64, Failure> {
    let r = b
        .query(&format!("SELECT COUNT(*) FROM {table}"), &[])
        .await?;
    match r.first().and_then(|r| r.int(0)) {
        Some(n) => Ok(n),
        None => wrong("COUNT(*) did not come back as an integer"),
    }
}

async fn transaction<R: RelationalBackend>(b: &R, s: &Sql) -> Result<(), Failure> {
    const T: &str = "persistant_rel_tx";
    drop_tables(b, &[T]).await?;
    b.execute(
        &format!(
            "CREATE TABLE {T} ({}, n BIGINT NOT NULL){}",
            s.pk("id"),
            s.tail()
        ),
        &[],
    )
    .await?;
    let ins = format!("INSERT INTO {T} (id, n) VALUES ({}, {})", s.p(1), s.p(2));
    let result = async {
        // Commit: visible.
        let mut tx = b.begin(Isolation::ReadCommitted).await?;
        tx.execute(&ins, &[Value::Int(1), Value::Int(10)]).await?;
        let own = tx.query(&format!("SELECT COUNT(*) FROM {T}"), &[]).await?;
        if own.first().and_then(|r| r.int(0)) != Some(1) {
            return wrong("a transaction cannot see its own write");
        }
        tx.commit().await?;
        if count(b, T).await? != 1 {
            return wrong("a committed write is not visible");
        }
        // Rollback: not visible.
        let mut tx = b.begin(Isolation::ReadCommitted).await?;
        tx.execute(&ins, &[Value::Int(2), Value::Int(20)]).await?;
        tx.rollback().await?;
        if count(b, T).await? != 1 {
            return wrong("a rolled-back write is visible");
        }
        // Dropped without commit: rolled back.
        {
            let mut tx = b.begin(Isolation::ReadCommitted).await?;
            tx.execute(&ins, &[Value::Int(3), Value::Int(30)]).await?;
        }
        if count(b, T).await? != 1 {
            return wrong("a transaction dropped without commit was not rolled back");
        }
        Ok(())
    }
    .await;
    drop_tables(b, &[T]).await?;
    result
}

async fn no_dirty_read<R: RelationalBackend>(b: &R, s: &Sql) -> Result<(), Failure> {
    const T: &str = "persistant_rel_isolation";
    drop_tables(b, &[T]).await?;
    b.execute(
        &format!(
            "CREATE TABLE {T} ({}, n BIGINT NOT NULL){}",
            s.pk("id"),
            s.tail()
        ),
        &[],
    )
    .await?;
    let ins = format!("INSERT INTO {T} (id, n) VALUES ({}, {})", s.p(1), s.p(2));
    let result = async {
        let mut tx = b.begin(Isolation::ReadCommitted).await?;
        tx.execute(&ins, &[Value::Int(1), Value::Int(1)]).await?;
        // `b.query` runs on another connection, outside `tx`.
        let outside = count(b, T).await?;
        if outside != 0 {
            let _ = tx.rollback().await;
            return wrong(format!(
                "another connection saw {outside} uncommitted row(s) (dirty read)"
            ));
        }
        tx.commit().await?;
        if count(b, T).await? != 1 {
            return wrong("a committed write is not visible to another connection");
        }
        Ok(())
    }
    .await;
    drop_tables(b, &[T]).await?;
    result
}

/// SQLite's `begin` is `BEGIN IMMEDIATE`: it holds the write lock, so the probes that need another
/// connection to commit (or a second transaction to run) in the meantime cannot run.
fn no_concurrent_commit(s: &Sql) -> Result<(), Failure> {
    if matches!(s.0, Dialect::Sqlite) {
        return Err(Failure::Backend(RelError::Unsupported(
            "begin() is BEGIN IMMEDIATE: it holds the write lock, so no other connection can commit during the transaction and the probe cannot run"
                .to_string(),
        )));
    }
    Ok(())
}

/// Run one transaction's last two steps. `Ok(true)` committed, `Ok(false)` was refused with a
/// serialization failure (at the update or at the commit); any other error is the caller's.
async fn finish<T: RelationalTx>(mut tx: T, sql: &str) -> Result<bool, Failure> {
    match tx.execute(sql, &[]).await {
        Ok(_) => {}
        Err(RelError::Serialization) => {
            let _ = tx.rollback().await;
            return Ok(false);
        }
        Err(e) => {
            let _ = tx.rollback().await;
            return Err(e.into());
        }
    }
    match tx.commit().await {
        Ok(()) => Ok(true),
        Err(RelError::Serialization) => Ok(false),
        Err(e) => Err(e.into()),
    }
}

/// Two transactions at `level` each read how many rows have `n = 1` (two), then each sets a different
/// row to 0 (write skew). Returns whether each committed.
async fn write_skew<R: RelationalBackend>(
    b: &R,
    s: &Sql,
    table: &str,
    level: Isolation,
) -> Result<(bool, bool), Failure> {
    b.execute(&format!("DELETE FROM {table}"), &[]).await?;
    let ins = format!("INSERT INTO {table} (id, n) VALUES ({}, 1)", s.p(1));
    b.execute(&ins, &[Value::Int(1)]).await?;
    b.execute(&ins, &[Value::Int(2)]).await?;
    let read = format!("SELECT COUNT(*) FROM {table} WHERE n = 1");
    let mut t1 = b.begin(level).await?;
    let mut t2 = b.begin(level).await?;
    t1.query(&read, &[]).await?;
    t2.query(&read, &[]).await?;
    let c1 = finish(t1, &format!("UPDATE {table} SET n = 0 WHERE id = 1")).await?;
    let c2 = finish(t2, &format!("UPDATE {table} SET n = 0 WHERE id = 2")).await?;
    Ok((c1, c2))
}

async fn serializable<R: RelationalBackend>(b: &R, s: &Sql) -> Result<(), Failure> {
    const T: &str = "persistant_rel_serializable";
    match s.0 {
        Dialect::Postgres | Dialect::MySql | Dialect::MariaDb => {}
        _ => {
            no_concurrent_commit(s)?;
            return Err(Failure::Backend(RelError::Unsupported(format!(
                "no probe for {}",
                s.0
            ))));
        }
    }
    drop_tables(b, &[T]).await?;
    b.execute(
        &format!(
            "CREATE TABLE {T} ({}, n BIGINT NOT NULL){}",
            s.pk("id"),
            s.tail()
        ),
        &[],
    )
    .await?;
    let result = if matches!(s.0, Dialect::Postgres) {
        async {
            // Control: at read committed the schedule does commit both, so the probe can see the anomaly.
            let control = write_skew(b, s, T, Isolation::ReadCommitted).await?;
            if control != (true, true) {
                return wrong(format!(
                    "control: at read committed the write-skew schedule committed {control:?}, not both"
                ));
            }
            let (c1, c2) = write_skew(b, s, T, Isolation::Serializable).await?;
            if c1 && c2 {
                return wrong("two serializable transactions that each read both rows and wrote a different one both committed (write skew)");
            }
            if !c1 && !c2 {
                return wrong("both serializable transactions were refused: one should have committed");
            }
            Ok(())
        }
        .await
    } else {
        async {
            b.execute(
                &format!("INSERT INTO {T} (id, n) VALUES ({}, 1)", s.p(1)),
                &[Value::Int(1)],
            )
            .await?;
            let read = format!("SELECT n FROM {T} WHERE id = 1");
            let nowait = format!("SELECT id FROM {T} WHERE id = 1 FOR UPDATE NOWAIT");
            // Control: after a plain read at read committed the row is not locked.
            let mut rc = b.begin(Isolation::ReadCommitted).await?;
            rc.query(&read, &[]).await?;
            let free = b.query(&nowait, &[]).await;
            let _ = rc.rollback().await;
            if !matches!(&free, Ok(rows) if rows.len() == 1) {
                return wrong(format!(
                    "control: a plain read at read committed left the row locked ({free:?})"
                ));
            }
            let mut tx = b.begin(Isolation::Serializable).await?;
            tx.query(&read, &[]).await?;
            let held = b.query(&nowait, &[]).await;
            let _ = tx.rollback().await;
            if held.is_ok() {
                return wrong(
                    "a plain read in a serializable transaction did not lock the row it read",
                );
            }
            match b.query(&nowait, &[]).await {
                Ok(rows) if rows.len() == 1 => Ok(()),
                other => wrong(format!(
                    "the row is still locked after the serializable transaction ended ({other:?})"
                )),
            }
        }
        .await
    };
    drop_tables(b, &[T]).await?;
    result
}

async fn repeatable_read<R: RelationalBackend>(b: &R, s: &Sql) -> Result<(), Failure> {
    const T: &str = "persistant_rel_repeatable";
    no_concurrent_commit(s)?;
    drop_tables(b, &[T]).await?;
    b.execute(
        &format!(
            "CREATE TABLE {T} ({}, n BIGINT NOT NULL){}",
            s.pk("id"),
            s.tail()
        ),
        &[],
    )
    .await?;
    let ins = format!("INSERT INTO {T} (id, n) VALUES ({}, {})", s.p(1), s.p(2));
    let read = format!("SELECT n FROM {T} WHERE id = 1");
    let result = async {
        b.execute(&ins, &[Value::Int(1), Value::Int(1)]).await?;
        let mut tx = b.begin(Isolation::RepeatableRead).await?;
        let first = tx.query(&read, &[]).await?;
        if first.first().and_then(|r| r.int(0)) != Some(1) {
            let _ = tx.rollback().await;
            return wrong("the first read did not return the row written before the transaction");
        }
        // `b.execute` runs on another connection, outside `tx`, and commits at once.
        b.execute(
            &format!("UPDATE {T} SET n = 2 WHERE id = 1"),
            &[],
        )
        .await?;
        b.execute(&ins, &[Value::Int(2), Value::Int(2)]).await?;
        let again = tx.query(&read, &[]).await?;
        let seen = again.first().and_then(|r| r.int(0));
        if seen != Some(1) {
            let _ = tx.rollback().await;
            return wrong(format!(
                "a row read twice in one transaction changed from 1 to {seen:?} (non-repeatable read)"
            ));
        }
        let rows = tx
            .query(&format!("SELECT COUNT(*) FROM {T}"), &[])
            .await?;
        let n = rows.first().and_then(|r| r.int(0));
        if n != Some(1) {
            let _ = tx.rollback().await;
            return wrong(format!(
                "a table counted inside one transaction went from 1 to {n:?} rows (phantom read)"
            ));
        }
        tx.commit().await?;
        let after = b.query(&read, &[]).await?;
        if after.first().and_then(|r| r.int(0)) != Some(2) {
            return wrong("the other connection's committed update is not visible after the transaction ends");
        }
        Ok(())
    }
    .await;
    drop_tables(b, &[T]).await?;
    result
}

async fn savepoint<R: RelationalBackend>(b: &R, s: &Sql) -> Result<(), Failure> {
    const T: &str = "persistant_rel_savepoint";
    drop_tables(b, &[T]).await?;
    b.execute(
        &format!(
            "CREATE TABLE {T} ({}, n BIGINT NOT NULL){}",
            s.pk("id"),
            s.tail()
        ),
        &[],
    )
    .await?;
    let ins = format!("INSERT INTO {T} (id, n) VALUES ({}, {})", s.p(1), s.p(2));
    let result = async {
        let mut tx = b.begin(Isolation::ReadCommitted).await?;
        tx.execute(&ins, &[Value::Int(1), Value::Int(1)]).await?;
        tx.execute("SAVEPOINT persistant_sp", &[]).await?;
        tx.execute(&ins, &[Value::Int(2), Value::Int(2)]).await?;
        tx.execute("ROLLBACK TO SAVEPOINT persistant_sp", &[])
            .await?;
        tx.commit().await?;
        let rows = b
            .query(&format!("SELECT id FROM {T} ORDER BY id"), &[])
            .await?;
        let ids: Vec<Option<i64>> = rows.iter().map(|r| r.int(0)).collect();
        if ids != [Some(1)] {
            return wrong(format!(
                "after ROLLBACK TO a savepoint and commit, expected only row 1, found {ids:?}"
            ));
        }
        Ok(())
    }
    .await;
    drop_tables(b, &[T]).await?;
    result
}

async fn row_lock<R: RelationalBackend>(b: &R, s: &Sql) -> Result<(), Failure> {
    const T: &str = "persistant_rel_row_lock";
    if matches!(s.0, Dialect::Sqlite) {
        return Err(Failure::Backend(RelError::Unsupported(
            "SQLite has no row locks: a write locks the whole database".to_string(),
        )));
    }
    drop_tables(b, &[T]).await?;
    b.execute(
        &format!(
            "CREATE TABLE {T} ({}, n BIGINT NOT NULL){}",
            s.pk("id"),
            s.tail()
        ),
        &[],
    )
    .await?;
    b.execute(
        &format!("INSERT INTO {T} (id, n) VALUES ({}, {})", s.p(1), s.p(2)),
        &[Value::Int(1), Value::Int(1)],
    )
    .await?;
    let lock = format!("SELECT id FROM {T} WHERE id = 1 FOR UPDATE");
    let probe = format!("SELECT id FROM {T} WHERE id = 1 FOR UPDATE SKIP LOCKED");
    let result = async {
        let mut tx = b.begin(Isolation::ReadCommitted).await?;
        if tx.query(&lock, &[]).await?.len() != 1 {
            return wrong("FOR UPDATE did not return the row it locks");
        }
        // `b.query` runs on another connection, outside `tx`.
        let held = b.query(&probe, &[]).await;
        let held = match held {
            Ok(rows) => rows.len(),
            Err(e) => {
                let _ = tx.rollback().await;
                return Err(e.into());
            }
        };
        if held != 0 {
            let _ = tx.rollback().await;
            return wrong("another connection locked a row an open transaction holds FOR UPDATE");
        }
        tx.commit().await?;
        if b.query(&probe, &[]).await?.len() != 1 {
            return wrong("the row is still locked after the transaction committed");
        }
        Ok(())
    }
    .await;
    drop_tables(b, &[T]).await?;
    result
}

/// `constrained = false` drops the `WHERE`: the known-bad control, a backend that ignores the condition.
async fn upsert_where<R: RelationalBackend>(
    b: &R,
    s: &Sql,
    constrained: bool,
) -> Result<(), Failure> {
    const T: &str = "persistant_rel_upsert";
    if matches!(s.0, Dialect::MySql | Dialect::MariaDb) {
        return Err(Failure::Backend(RelError::Unsupported(
            "no ON CONFLICT .. DO UPDATE .. WHERE clause".to_string(),
        )));
    }
    drop_tables(b, &[T]).await?;
    b.execute(
        &format!(
            "CREATE TABLE {T} ({}, n BIGINT NOT NULL){}",
            s.pk("id"),
            s.tail()
        ),
        &[],
    )
    .await?;
    let cond = if constrained {
        format!(" WHERE {T}.n < EXCLUDED.n")
    } else {
        String::new()
    };
    let up = format!(
        "INSERT INTO {T} (id, n) VALUES ({}, {}) ON CONFLICT (id) DO UPDATE SET n = EXCLUDED.n{cond}",
        s.p(1),
        s.p(2)
    );
    let result = async {
        // Absent: inserted. Then a lower value is refused by the WHERE, a higher one stored.
        for (n, want) in [(10, 10), (5, 10), (20, 20)] {
            b.execute(&up, &[Value::Int(1), Value::Int(n)]).await?;
            let rows = b
                .query(&format!("SELECT n FROM {T} WHERE id = 1"), &[])
                .await?;
            let got = rows.first().and_then(|r| r.int(0));
            if got != Some(want) {
                return wrong(format!(
                    "upsert of {n} left {got:?}, expected {want} (the WHERE must hold the row when n is not higher)"
                ));
            }
        }
        Ok(())
    }
    .await;
    drop_tables(b, &[T]).await?;
    result
}

async fn unique<R: RelationalBackend>(b: &R, s: &Sql) -> Result<(), Failure> {
    const T: &str = "persistant_rel_unique";
    drop_tables(b, &[T]).await?;
    b.execute(
        &format!(
            "CREATE TABLE {T} ({}, n BIGINT NOT NULL){}",
            s.pk("id"),
            s.tail()
        ),
        &[],
    )
    .await?;
    let ins = format!("INSERT INTO {T} (id, n) VALUES ({}, {})", s.p(1), s.p(2));
    let result = async {
        b.execute(&ins, &[Value::Int(1), Value::Int(1)]).await?;
        match b.execute(&ins, &[Value::Int(1), Value::Int(2)]).await {
            Err(RelError::UniqueViolation { .. }) => {}
            other => return wrong(format!("a duplicate key came back as {other:?}")),
        }
        // Inside a transaction too.
        let mut tx = b.begin(Isolation::ReadCommitted).await?;
        let dup = tx.execute(&ins, &[Value::Int(1), Value::Int(3)]).await;
        let _ = tx.rollback().await;
        match dup {
            Err(RelError::UniqueViolation { .. }) => Ok(()),
            other => wrong(format!(
                "a duplicate key in a transaction came back as {other:?}"
            )),
        }
    }
    .await;
    drop_tables(b, &[T]).await?;
    result
}

async fn foreign_key<R: RelationalBackend>(
    b: &R,
    s: &Sql,
    constrained: bool,
) -> Result<(), Failure> {
    const P: &str = "persistant_rel_fk_parent";
    const C: &str = "persistant_rel_fk_child";
    drop_tables(b, &[C, P]).await?;
    b.execute(
        &format!("CREATE TABLE {P} ({}){}", s.pk("id"), s.tail()),
        &[],
    )
    .await?;
    let refs = if constrained {
        format!(", FOREIGN KEY (pid) REFERENCES {P}(id)")
    } else {
        String::new()
    };
    b.execute(
        &format!(
            "CREATE TABLE {C} ({}, pid BIGINT NOT NULL{refs}){}",
            s.pk("id"),
            s.tail()
        ),
        &[],
    )
    .await?;
    let result = async {
        b.execute(
            &format!("INSERT INTO {P} (id) VALUES ({})", s.p(1)),
            &[Value::Int(1)],
        )
        .await?;
        let ins = format!("INSERT INTO {C} (id, pid) VALUES ({}, {})", s.p(1), s.p(2));
        b.execute(&ins, &[Value::Int(1), Value::Int(1)]).await?;
        match b.execute(&ins, &[Value::Int(2), Value::Int(99)]).await {
            Err(RelError::ForeignKeyViolation { .. }) => {}
            Ok(_) => return wrong("a reference to a missing row was accepted"),
            other => {
                return wrong(format!(
                    "a reference to a missing row came back as {other:?}"
                ));
            }
        }
        match b
            .execute(
                &format!("DELETE FROM {P} WHERE id = {}", s.p(1)),
                &[Value::Int(1)],
            )
            .await
        {
            Err(RelError::ForeignKeyViolation { .. }) => Ok(()),
            Ok(_) => wrong("a referenced row was deleted"),
            other => wrong(format!("deleting a referenced row came back as {other:?}")),
        }
    }
    .await;
    drop_tables(b, &[C, P]).await?;
    result
}

async fn check<R: RelationalBackend>(b: &R, s: &Sql, constrained: bool) -> Result<(), Failure> {
    const T: &str = "persistant_rel_check";
    drop_tables(b, &[T]).await?;
    let ck = if constrained { " CHECK (n >= 0)" } else { "" };
    b.execute(
        &format!(
            "CREATE TABLE {T} ({}, n BIGINT NOT NULL{ck}){}",
            s.pk("id"),
            s.tail()
        ),
        &[],
    )
    .await?;
    let ins = format!("INSERT INTO {T} (id, n) VALUES ({}, {})", s.p(1), s.p(2));
    let result = async {
        b.execute(&ins, &[Value::Int(1), Value::Int(0)]).await?;
        match b.execute(&ins, &[Value::Int(2), Value::Int(-1)]).await {
            Err(RelError::CheckViolation { .. }) => Ok(()),
            Ok(_) => wrong("a row violating a CHECK was accepted"),
            other => wrong(format!("a row violating a CHECK came back as {other:?}")),
        }
    }
    .await;
    drop_tables(b, &[T]).await?;
    result
}

// ---- certification --------------------------------------------------------------------------

/// The outcome of certifying one leaf.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Certified {
    /// The leaf.
    pub leaf: Leaf,
    /// The row to record.
    pub row: MatrixRow,
}

/// Run every leaf here against `target` and return one row per leaf.
///
/// A leaf is **on** only if the target passed *and* every known-bad control failed (see the module
/// docs for the positive control). `config` is the facts a gate can read for this target; `metadata`
/// is copied into every row with `test=rel::<leaf>` added.
pub async fn certify<R: RelationalBackend>(
    target: &R,
    config: &Config,
    metadata: &Config,
) -> Vec<Certified> {
    certify_leaves(target, config, metadata, &Leaf::ALL).await
}

/// As [`certify`], for the given leaves only (the eligibility check of a consumer that needs a few).
pub async fn certify_leaves<R: RelationalBackend>(
    target: &R,
    config: &Config,
    metadata: &Config,
    leaves: &[Leaf],
) -> Vec<Certified> {
    let mut out = Vec::new();
    for leaf in leaves.iter().copied() {
        let result = run_leaf(leaf, target, true).await;
        let positive_passed = !matches!(
            &result,
            Err(Failure::Backend(
                RelError::Unsupported(_) | RelError::Unavailable(_)
            ))
        );
        let mut known_bad_failed = true;
        for d in leaf.defects() {
            let bad = Defective::new(target, *d);
            if run_leaf(leaf, &bad, true).await.is_ok() {
                known_bad_failed = false;
            }
        }
        if leaf.has_unconstrained_control() && run_leaf(leaf, target, false).await.is_ok() {
            known_bad_failed = false;
        }
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
        out.push(Certified { leaf, row });
    }
    out
}
