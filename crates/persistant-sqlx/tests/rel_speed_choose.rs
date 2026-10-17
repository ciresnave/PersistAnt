// SPDX-License-Identifier: MIT OR Apache-2.0
//! Stages 1, 3 and 4 of backend benchmarking for the relational kind, on a real SQLite file: the
//! workloads are timed only on a backend that passes the leaf they depend on, a backend that fails it
//! gets no number, and the contract check feeds `choose` end to end.
#![cfg(feature = "sqlite")]

use persistant_core::{Config, Reason};
use persistant_relational::choose::{self, Durability, Why};
use persistant_relational::conformance::{Defect, Defective, Leaf};
use persistant_relational::speed::{Environment, Outcome, SpeedTable, Workload, run};
use persistant_relational::{RelationalBackend, Value};
use persistant_sqlx::SqliteBackend;

async fn open() -> (SqliteBackend, std::path::PathBuf) {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path =
        std::env::temp_dir().join(format!("persistant-relspeed-{}-{n}.db", std::process::id()));
    (SqliteBackend::open(&path).await.expect("open"), path)
}

fn cleanup(path: &std::path::Path) {
    for ext in ["", "-wal", "-shm"] {
        let mut p = path.as_os_str().to_owned();
        p.push(ext);
        let _ = std::fs::remove_file(p);
    }
}

fn env() -> Environment {
    Environment::detect("cargo test -p persistant-sqlx --features sqlite", None)
}

async fn tables_named(db: &SqliteBackend, name: &str) -> Option<i64> {
    db.query(
        "SELECT COUNT(*) FROM sqlite_master WHERE name = ?1",
        &[Value::Text(name.to_string())],
    )
    .await
    .unwrap()[0]
        .int(0)
}

#[tokio::test]
async fn every_workload_is_measured_on_a_correct_backend_and_the_table_is_left_as_found() {
    let (db, path) = open().await;
    let rows = run(&db, &Config::new(), &env(), &Workload::ALL, 20).await;
    assert_eq!(rows.len(), Workload::ALL.len());
    for r in &rows {
        let Outcome::Measured(s) = &r.outcome else {
            panic!("{}: {:?}", r.workload, r.outcome)
        };
        assert_eq!(s.n, 20, "{}", r.workload);
        assert!(s.p50_ns <= s.p95_ns && s.p95_ns <= s.p99_ns && s.p99_ns <= s.max_ns);
    }
    let t = SpeedTable::from_rows(rows);
    assert_eq!(SpeedTable::parse(&t.to_tsv()).unwrap(), t);
    // The workloads table is gone (control: a table that exists is found by the same query).
    db.execute("CREATE TABLE persistant_speed_control (id INTEGER)", &[])
        .await
        .unwrap();
    assert_eq!(tables_named(&db, "persistant_speed_control").await, Some(1));
    assert_eq!(tables_named(&db, "persistant_speed_rel").await, Some(0));
    drop(db);
    cleanup(&path);
}

#[tokio::test]
async fn a_backend_that_fails_a_leaf_gets_no_number_for_the_workload_that_needs_it() {
    let (db, path) = open().await;
    let bad = Defective::new(&db, Defect::NoTransactions);
    let rows = run(
        &bad,
        &Config::new(),
        &env(),
        &[Workload::Transaction, Workload::Insert],
        5,
    )
    .await;
    assert!(
        matches!(
            &rows[0].outcome,
            Outcome::Excluded {
                reason: Reason::Wrong,
                leaf,
                ..
            } if leaf == "rel.transaction"
        ),
        "{:?}",
        rows[0].outcome
    );
    // Control: a workload that depends on a leaf the defect does not touch is still measured, and the
    // same workload on the correct backend is measured.
    assert!(
        matches!(rows[1].outcome, Outcome::Measured(_)),
        "{:?}",
        rows[1].outcome
    );
    let good = run(&db, &Config::new(), &env(), &[Workload::Transaction], 5).await;
    assert!(matches!(good[0].outcome, Outcome::Measured(_)));
    drop(db);
    cleanup(&path);
}

#[tokio::test]
async fn the_contract_check_feeds_the_choice_end_to_end() {
    let (db, path) = open().await;
    let e = env();
    let bad = Defective::new(&db, Defect::NoTransactions);
    let leaves = [Leaf::Values, Leaf::Transaction];
    let cands = [
        choose::candidate(&db, &Config::new(), &leaves).await,
        choose::candidate(&bad, &Config::new(), &leaves).await,
    ];
    let table = SpeedTable::from_rows(run(&db, &Config::new(), &e, &[Workload::Insert], 10).await);
    let n = choose::need(
        &leaves,
        Durability::Process,
        Workload::Insert,
        e.comparable_key(),
    );
    let c = choose::choose(&cands, &n, &table).unwrap();
    assert_eq!(c.ranked.len(), 1);
    assert_eq!(c.excluded.len(), 1);
    assert!(matches!(&c.excluded[0].why, Why::Contract(f) if f[0].reason == Reason::Wrong));
    let log = c.log_lines().join("\n");
    assert!(log.contains("verified: no"), "{log}");
    // A need above what SQLite declares excludes it by durability, not by speed.
    let n = choose::need(
        &leaves,
        Durability::Machine,
        Workload::Insert,
        e.comparable_key(),
    );
    assert!(choose::choose(&cands[..1], &n, &table).is_err());
    drop(db);
    cleanup(&path);
}
