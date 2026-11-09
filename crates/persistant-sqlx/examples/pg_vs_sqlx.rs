// SPDX-License-Identifier: MIT OR Apache-2.0
//! What PersistAnt's PostgreSQL layer costs over calling sqlx directly (ROADMAP row 30; auth-framework
//! asked for "a Postgres throughput figure").
//!
//! The same statements, on the **same pool**, over the same table, are timed twice: through
//! [`PgBackend`] (`RelationalBackend::execute` / `query` / `begin`) and through `sqlx::query` on
//! `PgBackend::pool()`. Nothing else differs, so the gap is the layer: value conversion, parameter
//! binding by type, row conversion and error classification. The workloads are the relational speed
//! workloads that map onto one statement (insert, select by key, update) and the one-insert
//! transaction.
//!
//! Each workload runs warm-up first (discarded), then direct, wrapped, wrapped, direct (ABBA), and the
//! samples of the two runs of one path are pooled, so a drift in the server's state favours neither.
//!
//! `PERSISTANT_TEST_DATABASE_URL=postgres://... cargo run -q --release -p persistant-sqlx --example pg_vs_sqlx`
//!
//! Output is a tab-separated table on stdout (`workload path n p50_ns p95_ns ops_per_sec`) followed by
//! one `ratio` line per workload. **The figures hold on the machine and server that produced them**
//! (a database in a container includes the network hop to it): the *ratio* is the finding, not the
//! absolute numbers, and nothing compares either to a constant.

use persistant_core::bench::Stats;
use persistant_relational::{Isolation, RelationalBackend, RelationalTx, Value};
use persistant_sqlx::PgBackend;
use std::time::Instant;

const TABLE: &str = "persistant_pg_vs_sqlx";
const SEED: i64 = 100;
const WARMUP: i64 = 50;
const N: i64 = 400;

#[derive(Clone, Copy, PartialEq)]
enum W {
    Insert,
    Select,
    Update,
    Tx,
}

impl W {
    const ALL: [W; 4] = [W::Insert, W::Select, W::Update, W::Tx];
    fn id(self) -> &'static str {
        match self {
            W::Insert => "insert",
            W::Select => "select_by_key",
            W::Update => "update",
            W::Tx => "transaction",
        }
    }
}

fn text() -> String {
    "x".repeat(64)
}

const INS: &str = "INSERT INTO persistant_pg_vs_sqlx (id, n, t) VALUES ($1, $2, $3)";
const SEL: &str = "SELECT n, t FROM persistant_pg_vs_sqlx WHERE id = $1";
const UPD: &str = "UPDATE persistant_pg_vs_sqlx SET n = n + 1 WHERE id = $1";

/// One operation straight on sqlx. `id` is a fresh key for the inserting workloads.
async fn direct(pg: &PgBackend, w: W, i: i64, id: i64) {
    let pool = pg.pool();
    match w {
        W::Insert => {
            sqlx::query(INS)
                .bind(id)
                .bind(id)
                .bind(text())
                .execute(pool)
                .await
                .unwrap();
        }
        W::Select => {
            let rows = sqlx::query(SEL)
                .bind(i % SEED)
                .fetch_all(pool)
                .await
                .unwrap();
            assert_eq!(rows.len(), 1);
        }
        W::Update => {
            let r = sqlx::query(UPD).bind(i % SEED).execute(pool).await.unwrap();
            assert_eq!(r.rows_affected(), 1);
        }
        W::Tx => {
            let mut tx = pool.begin().await.unwrap();
            sqlx::query(INS)
                .bind(id)
                .bind(id)
                .bind(text())
                .execute(&mut *tx)
                .await
                .unwrap();
            tx.commit().await.unwrap();
        }
    }
}

/// The same operation through the PersistAnt layer.
async fn wrapped(pg: &PgBackend, w: W, i: i64, id: i64) {
    match w {
        W::Insert => {
            pg.execute(INS, &[Value::Int(id), Value::Int(id), Value::Text(text())])
                .await
                .unwrap();
        }
        W::Select => {
            let rows = pg.query(SEL, &[Value::Int(i % SEED)]).await.unwrap();
            assert_eq!(rows.len(), 1);
        }
        W::Update => {
            let n = pg.execute(UPD, &[Value::Int(i % SEED)]).await.unwrap();
            assert_eq!(n, 1);
        }
        W::Tx => {
            let mut tx = pg.begin(Isolation::ReadCommitted).await.unwrap();
            tx.execute(INS, &[Value::Int(id), Value::Int(id), Value::Text(text())])
                .await
                .unwrap();
            tx.commit().await.unwrap();
        }
    }
}

/// Run `count` operations and return each one's duration in nanoseconds. Keys for the inserting
/// workloads start at `*next` and advance, so no two operations (of any path) share one.
async fn run(pg: &PgBackend, w: W, path_direct: bool, count: i64, next: &mut i64) -> Vec<u64> {
    let mut out = Vec::with_capacity(count as usize);
    for i in 0..count {
        let id = *next;
        *next += 1;
        let t = Instant::now();
        if path_direct {
            direct(pg, w, i, id).await;
        } else {
            wrapped(pg, w, i, id).await;
        }
        out.push(u64::try_from(t.elapsed().as_nanos()).unwrap_or(u64::MAX));
    }
    out
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let url = std::env::var("PERSISTANT_TEST_DATABASE_URL")
        .expect("set PERSISTANT_TEST_DATABASE_URL to a PostgreSQL server");
    let pg = PgBackend::connect(&url).await.expect("connect");
    let _ = pg.execute(&format!("DROP TABLE {TABLE}"), &[]).await;
    pg.execute(
        &format!("CREATE TABLE {TABLE} (id BIGINT NOT NULL PRIMARY KEY, n BIGINT NOT NULL, t TEXT NOT NULL)"),
        &[],
    )
    .await
    .unwrap();
    for id in 0..SEED {
        pg.execute(INS, &[Value::Int(id), Value::Int(id), Value::Text(text())])
            .await
            .unwrap();
    }
    println!(
        "# postgres {}; {N} operations per run, ABBA, samples pooled",
        pg.version()
    );
    println!("workload\tpath\tn\tp50_ns\tp95_ns\tops_per_sec");
    let mut next = 1_000_000_i64;
    let mut ratios = Vec::new();
    for w in W::ALL {
        run(&pg, w, true, WARMUP, &mut next).await;
        run(&pg, w, false, WARMUP, &mut next).await;
        let mut d = run(&pg, w, true, N, &mut next).await;
        let mut r = run(&pg, w, false, N, &mut next).await;
        r.extend(run(&pg, w, false, N, &mut next).await);
        d.extend(run(&pg, w, true, N, &mut next).await);
        let (d, r) = (
            Stats::from_samples(d).unwrap(),
            Stats::from_samples(r).unwrap(),
        );
        for (path, s) in [("direct_sqlx", &d), ("persistant", &r)] {
            println!(
                "{}\t{path}\t{}\t{}\t{}\t{}",
                w.id(),
                s.n,
                s.p50_ns,
                s.p95_ns,
                s.ops_per_sec
            );
        }
        ratios.push(format!(
            "ratio\t{}\tp50 {:.3}\tp95 {:.3}\tops {:.3}",
            w.id(),
            r.p50_ns as f64 / d.p50_ns as f64,
            r.p95_ns as f64 / d.p95_ns as f64,
            r.ops_per_sec as f64 / d.ops_per_sec as f64,
        ));
    }
    println!(
        "# ratio = persistant / direct_sqlx (p50, p95: above 1 is slower; ops: below 1 is slower)"
    );
    for line in ratios {
        println!("{line}");
    }
    let _ = pg.execute(&format!("DROP TABLE {TABLE}"), &[]).await;
}
