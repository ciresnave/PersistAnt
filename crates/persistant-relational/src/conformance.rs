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

use crate::{Dialect, RelError, RelationalBackend, RelationalTx, Row, Value};
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
}

impl Leaf {
    /// Every leaf with a test here.
    pub const ALL: [Leaf; 6] = [
        Leaf::Values,
        Leaf::Transaction,
        Leaf::UniqueClassified,
        Leaf::ForeignKey,
        Leaf::CheckConstraint,
        Leaf::IsolationNoDirtyRead,
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
        }
    }

    fn defects(self) -> &'static [Defect] {
        match self {
            Leaf::Values => &[Defect::LossyBytes],
            Leaf::Transaction | Leaf::IsolationNoDirtyRead => &[Defect::NoTransactions],
            Leaf::UniqueClassified => &[Defect::UnclassifiedErrors],
            Leaf::ForeignKey | Leaf::CheckConstraint => &[],
        }
    }

    /// Whether the leaf has a "schema without the constraint" known-bad run.
    fn has_unconstrained_control(self) -> bool {
        matches!(self, Leaf::ForeignKey | Leaf::CheckConstraint)
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

    async fn begin(&self) -> Result<DefectiveTx<'_, R>, RelError> {
        if self.defect == Defect::NoTransactions {
            return Ok(DefectiveTx::Direct(self.inner));
        }
        Ok(DefectiveTx::Real(
            self.inner
                .begin()
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
enum Ty {
    Int,
    Text,
    Bytes,
    Float,
}

struct Sql(Dialect);

impl Sql {
    /// The placeholder for parameter `n` (1-based).
    fn p(&self, n: usize) -> String {
        match self.0 {
            Dialect::Postgres => format!("${n}"),
            Dialect::Sqlite => format!("?{n}"),
            _ => "?".to_string(),
        }
    }

    fn ty(&self, t: Ty) -> &'static str {
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
    fn tail(&self) -> &'static str {
        match self.0 {
            Dialect::MySql | Dialect::MariaDb => " ENGINE=InnoDB",
            _ => "",
        }
    }

    /// An integer primary key column definition.
    fn pk(&self, name: &str) -> String {
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

async fn drop_tables<R: RelationalBackend>(b: &R, names: &[&str]) -> Result<(), RelError> {
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
        let mut tx = b.begin().await?;
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
        let mut tx = b.begin().await?;
        tx.execute(&ins, &[Value::Int(2), Value::Int(20)]).await?;
        tx.rollback().await?;
        if count(b, T).await? != 1 {
            return wrong("a rolled-back write is visible");
        }
        // Dropped without commit: rolled back.
        {
            let mut tx = b.begin().await?;
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
        let mut tx = b.begin().await?;
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
        let mut tx = b.begin().await?;
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
    let mut out = Vec::new();
    for leaf in Leaf::ALL {
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
