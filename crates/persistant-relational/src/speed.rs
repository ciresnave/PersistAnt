// SPDX-License-Identifier: MIT OR Apache-2.0
//! Stage 3 of backend benchmarking for the relational kind: **speed**, as data, with the environment
//! recorded. The rules and the data types are the kind-agnostic ones in [`persistant_core::bench`]
//! (only a backend that passes the contract leaf a workload depends on is timed; the rest are listed
//! as excluded, never ranked; a figure carries its environment and the command that re-runs it).
//! This module owns the relational workloads and how they are timed: wall-clock, one statement at a
//! time, from the caller's side of the trait. A network database includes its round trip.

use crate::conformance::{Leaf, Sql, Ty, drop_tables};
use crate::contract;
use crate::{Dialect, RelError, RelationalBackend, RelationalTx, Value};
use persistant_core::choose::Eligibility;
use persistant_core::{Config, Reason};
use std::time::Instant;

pub use persistant_core::bench::{
    Comparison, Environment, Left, Outcome, Ranked, SpeedParseError as ParseError, SpeedRow,
    SpeedTable, Stats, compare,
};

/// One thing worth timing, with the contract leaf it depends on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum Workload {
    /// `INSERT` of one row to a fresh key, outside a transaction.
    Insert,
    /// `SELECT` of one row by primary key, over a table of 100 rows.
    SelectByKey,
    /// `UPDATE` of one row by primary key.
    Update,
    /// `begin`, one `INSERT`, `commit`.
    Transaction,
    /// Inside an open transaction: `SAVEPOINT`, one `INSERT`, `ROLLBACK TO SAVEPOINT` (three statements).
    Savepoint,
    /// `INSERT .. ON CONFLICT (id) DO UPDATE .. WHERE`, over 100 rows, always taking the update branch.
    UpsertWhere,
}

impl Workload {
    /// Every workload.
    pub const ALL: [Workload; 6] = [
        Workload::Insert,
        Workload::SelectByKey,
        Workload::Update,
        Workload::Transaction,
        Workload::Savepoint,
        Workload::UpsertWhere,
    ];

    /// The id used in data files.
    pub fn id(self) -> &'static str {
        match self {
            Workload::Insert => "speed.rel.insert",
            Workload::SelectByKey => "speed.rel.select_by_key",
            Workload::Update => "speed.rel.update",
            Workload::Transaction => "speed.rel.transaction",
            Workload::Savepoint => "speed.rel.savepoint",
            Workload::UpsertWhere => "speed.rel.upsert_where",
        }
    }

    /// The contract leaf the workload needs: no pass, no number.
    pub fn required(self) -> Leaf {
        match self {
            Workload::Insert | Workload::SelectByKey | Workload::Update => Leaf::Values,
            Workload::Transaction => Leaf::Transaction,
            Workload::Savepoint => Leaf::Savepoint,
            Workload::UpsertWhere => Leaf::UpsertWhere,
        }
    }
}

/// Time `workloads` on `target`, `iterations` operations each. A workload whose leaf the target does
/// not pass gets an excluded row and no timing.
pub async fn run<R: RelationalBackend>(
    target: &R,
    config: &Config,
    env: &Environment,
    workloads: &[Workload],
    iterations: u32,
) -> Vec<SpeedRow> {
    let mut out = Vec::with_capacity(workloads.len());
    for w in workloads.iter().copied() {
        let leaf = w.required();
        let outcome = match contract::check(target, &[leaf]).await {
            Eligibility::Eligible => match time(target, w, iterations).await {
                Ok(Some(s)) => Outcome::Measured(s),
                Ok(None) => Outcome::Excluded {
                    leaf: leaf.id().to_string(),
                    reason: Reason::Untested,
                    note: "no operations were timed".to_string(),
                },
                Err(e) => Outcome::Excluded {
                    leaf: leaf.id().to_string(),
                    reason: Reason::Missing,
                    note: format!("the workload failed on a backend that passed the leaf: {e}"),
                },
            },
            Eligibility::Excluded(ex) => {
                let e = &ex[0];
                Outcome::Excluded {
                    leaf: e.leaf.clone(),
                    reason: e.reason,
                    note: e.note.clone(),
                }
            }
        };
        out.push(SpeedRow {
            workload: w.id().to_string(),
            backend: target.backend().to_string(),
            version: target.version().to_string(),
            config: config.clone(),
            env: env.clone(),
            outcome,
        });
    }
    out
}

const TABLE: &str = "persistant_speed_rel";
const SEED_ROWS: i64 = 100;

async fn time<R: RelationalBackend>(
    b: &R,
    w: Workload,
    iterations: u32,
) -> Result<Option<Stats>, RelError> {
    let s = Sql(b.dialect());
    drop_tables(b, &[TABLE]).await?;
    b.execute(
        &format!(
            "CREATE TABLE {TABLE} ({}, n BIGINT NOT NULL, t {} NOT NULL){}",
            s.pk("id"),
            s.ty(Ty::Text),
            s.tail()
        ),
        &[],
    )
    .await?;
    let result = timed_loop(b, &s, w, iterations).await;
    // Best effort: leave a shared target as found, whatever happened.
    let _ = drop_tables(b, &[TABLE]).await;
    result
}

async fn timed_loop<R: RelationalBackend>(
    b: &R,
    s: &Sql,
    w: Workload,
    iterations: u32,
) -> Result<Option<Stats>, RelError> {
    let ins = format!(
        "INSERT INTO {TABLE} (id, n, t) VALUES ({}, {}, {})",
        s.p(1),
        s.p(2),
        s.p(3)
    );
    let sel = format!("SELECT n, t FROM {TABLE} WHERE id = {}", s.p(1));
    let upd = format!("UPDATE {TABLE} SET n = n + 1 WHERE id = {}", s.p(1));
    let row = |id: i64| [Value::Int(id), Value::Int(id), Value::Text("x".repeat(64))];
    // `n = id` for a seeded row, so an upsert of `n = 1000 + i` is always higher and always updates.
    let tail = if matches!(s.0, Dialect::MySql | Dialect::MariaDb) {
        format!("ON DUPLICATE KEY UPDATE n = IF({TABLE}.n < VALUES(n), VALUES(n), {TABLE}.n)")
    } else {
        format!("ON CONFLICT (id) DO UPDATE SET n = EXCLUDED.n WHERE {TABLE}.n < EXCLUDED.n")
    };
    let ups = format!(
        "INSERT INTO {TABLE} (id, n, t) VALUES ({}, {}, {}) {tail}",
        s.p(1),
        s.p(2),
        s.p(3)
    );
    if matches!(
        w,
        Workload::SelectByKey | Workload::Update | Workload::UpsertWhere
    ) {
        for id in 0..SEED_ROWS {
            b.execute(&ins, &row(id)).await?;
        }
    }
    let mut us: Vec<u64> = Vec::with_capacity(iterations as usize);
    macro_rules! timed {
        ($e:expr) => {{
            let t = Instant::now();
            let r = $e;
            us.push(u64::try_from(t.elapsed().as_nanos()).unwrap_or(u64::MAX));
            r
        }};
    }
    // One transaction for every iteration of the savepoint workload, rolled back at the end.
    let mut sp_tx = if w == Workload::Savepoint {
        Some(b.begin(crate::Isolation::ReadCommitted).await?)
    } else {
        None
    };
    for i in 0..i64::from(iterations) {
        match w {
            Workload::Insert => {
                timed!(b.execute(&ins, &row(i)).await?);
            }
            Workload::SelectByKey => {
                let rows = timed!(b.query(&sel, &[Value::Int(i % SEED_ROWS)]).await?);
                if rows.len() != 1 {
                    return Err(RelError::Backend(
                        "select by key did not return its row".to_string(),
                    ));
                }
            }
            Workload::Update => {
                let n = timed!(b.execute(&upd, &[Value::Int(i % SEED_ROWS)]).await?);
                if n != 1 {
                    return Err(RelError::Backend(
                        "update by key did not touch one row".to_string(),
                    ));
                }
            }
            Workload::Transaction => {
                let t = Instant::now();
                let mut tx = b.begin(crate::Isolation::ReadCommitted).await?;
                tx.execute(&ins, &row(i)).await?;
                tx.commit().await?;
                us.push(u64::try_from(t.elapsed().as_nanos()).unwrap_or(u64::MAX));
            }
            Workload::Savepoint => {
                let tx = sp_tx.as_mut().expect("opened above for this workload");
                let t = Instant::now();
                tx.execute("SAVEPOINT persistant_speed_sp", &[]).await?;
                tx.execute(&ins, &row(i)).await?;
                tx.execute("ROLLBACK TO SAVEPOINT persistant_speed_sp", &[])
                    .await?;
                us.push(u64::try_from(t.elapsed().as_nanos()).unwrap_or(u64::MAX));
            }
            Workload::UpsertWhere => {
                let id = i % SEED_ROWS;
                let r = [
                    Value::Int(id),
                    Value::Int(1000 + i),
                    Value::Text("x".repeat(64)),
                ];
                timed!(b.execute(&ups, &r).await?);
                let got = b
                    .query(
                        &format!("SELECT n FROM {TABLE} WHERE id = {}", s.p(1)),
                        &[Value::Int(id)],
                    )
                    .await?;
                if got.first().and_then(|r| r.int(0)) != Some(1000 + i) {
                    return Err(RelError::Backend(
                        "the upsert did not take the update branch".to_string(),
                    ));
                }
            }
        }
    }
    if let Some(tx) = sp_tx {
        tx.rollback().await?;
    }
    Ok(Stats::from_samples(us))
}
