// SPDX-License-Identifier: MIT OR Apache-2.0
//! The relational driver and `KvOnRelational` over a real SQLite file (statements written in
//! PostgreSQL SQL and translated by UniSQL). No server is needed, so nothing here can skip.
#![cfg(feature = "sqlite")]

use persistant_core::{Config, Reason};
use persistant_kv::conformance::certify;
use persistant_kv::{KvBackend, KvOnRelational};
use persistant_relational::{RelError, RelationalBackend, RelationalTx, Value};
use persistant_sqlx::SqliteBackend;

fn fresh_path(tag: &str) -> std::path::PathBuf {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("persistant-{tag}-{}-{n}.db", std::process::id()))
}

async fn open(tag: &str) -> (SqliteBackend, std::path::PathBuf) {
    let path = fresh_path(tag);
    (SqliteBackend::open(&path).await.expect("open"), path)
}

fn cleanup(path: &std::path::Path) {
    for ext in ["", "-wal", "-shm"] {
        let mut p = path.as_os_str().to_owned();
        p.push(ext);
        let _ = std::fs::remove_file(p);
    }
}

#[tokio::test]
async fn statements_parameters_and_values_round_trip() {
    let (db, path) = open("values").await;
    assert_eq!(db.backend(), "sqlite");
    assert!(db.version().starts_with("3."), "version {:?}", db.version());
    let rows = db
        .query(
            "SELECT ?1, ?2, ?3, ?4",
            &[
                Value::Int(-7),
                Value::Text("ünï".into()),
                Value::Bytes(vec![0, 255, 1]),
                Value::Float(1.5),
            ],
        )
        .await
        .unwrap();
    assert_eq!(
        rows[0].values,
        vec![
            Value::Int(-7),
            Value::Text("ünï".into()),
            Value::Bytes(vec![0, 255, 1]),
            Value::Float(1.5)
        ]
    );
    cleanup(&path);
}

#[tokio::test]
async fn errors_are_classified_and_a_dropped_transaction_rolls_back() {
    let (db, path) = open("errors").await;
    db.execute(
        "CREATE TABLE t (id INTEGER PRIMARY KEY, n INTEGER NOT NULL)",
        &[],
    )
    .await
    .unwrap();
    db.execute("INSERT INTO t VALUES (1, 1)", &[])
        .await
        .unwrap();
    let dup = db.execute("INSERT INTO t VALUES (1, 2)", &[]).await;
    assert!(
        matches!(dup, Err(RelError::UniqueViolation { .. })),
        "{dup:?}"
    );
    let null = db.execute("INSERT INTO t VALUES (2, NULL)", &[]).await;
    assert!(
        matches!(null, Err(RelError::CheckViolation { .. })),
        "{null:?}"
    );
    {
        let mut tx = db.begin().await.unwrap();
        tx.execute("INSERT INTO t VALUES (3, 3)", &[])
            .await
            .unwrap();
        // dropped without commit
    }
    let n = db.query("SELECT COUNT(*) FROM t", &[]).await.unwrap();
    assert_eq!(
        n[0].values,
        vec![Value::Int(1)],
        "a dropped transaction must roll back"
    );
    cleanup(&path);
}

#[tokio::test]
async fn kv_on_sqlite_certifies_every_leaf_on() {
    let (db, path) = open("kv").await;
    let kv = KvOnRelational::open(db).await.expect("open");
    kv.put("a", b"1".to_vec(), None).await.unwrap();
    assert_eq!(kv.get("a").await.unwrap().as_deref(), Some(&b"1"[..]));
    let config = Config::new().with("dialect", "sqlite");
    let rows = certify(&kv, &config, &Config::new()).await;
    let off: Vec<_> = rows
        .iter()
        .filter(|c| !c.row.enabled)
        .map(|c| format!("{}: {:?} {}", c.leaf.id(), c.row.reason, c.row.note))
        .collect();
    assert!(off.is_empty(), "leaves off on SQLite: {off:#?}");
    assert!(rows.iter().all(|c| c.row.reason != Some(Reason::Wrong)));
    cleanup(&path);
}

/// Stage 2: the sampled-concurrency leaves, one attempt per run, five runs. One wrong run in five
/// is a flaky contract, not a pass. (Real-time leaves are left out: a stalled CI machine would fail
/// them for the wrong reason; `certify` retries those.)
#[tokio::test]
async fn the_concurrency_leaves_are_exact_on_every_run() {
    use persistant_kv::conformance::Leaf;
    use persistant_kv::stats::measure;
    let kv = KvOnRelational::open(open("stats").await.0)
        .await
        .expect("open");
    let leaves = [
        Leaf::PutIfAbsent,
        Leaf::CompareAndSwap,
        Leaf::CompareAndDelete,
        Leaf::BatchRollback,
        Leaf::BatchConcurrentRead,
        Leaf::OptimisticSequence,
    ];
    for s in measure(&kv, &leaves, 5).await {
        assert!(
            s.is_exact(),
            "{}: {}/{} runs passed, {} wrong, {} errored; first failure: {:?}",
            s.leaf.id(),
            s.passed,
            s.runs,
            s.wrong,
            s.errored,
            s.first_failure
        );
    }
}

/// Stage 3 (see `persistant_kv::speed`): every workload is timed on SQLite, the file the example
/// prints parses back, and nothing is asserted about how fast: the gate for a backend is the
/// contract, and a figure holds only in the environment it was taken in.
#[tokio::test]
async fn every_workload_is_timed_and_the_data_file_round_trips() {
    use persistant_kv::speed::{Environment, Outcome, SpeedTable, Workload, run};
    let kv = KvOnRelational::open(open("speed").await.0)
        .await
        .expect("open");
    let env = Environment::detect("cargo test -p persistant-sqlx --features sqlite", None);
    let cfg = Config::new().with("dialect", "sqlite");
    let rows = run(&kv, &cfg, &env, &Workload::ALL, 20).await;
    assert_eq!(rows.len(), Workload::ALL.len());
    for r in &rows {
        match &r.outcome {
            Outcome::Measured(s) => {
                assert_eq!(s.n, 20, "{}", r.workload);
                assert!(
                    s.p50_ns <= s.p99_ns && s.p99_ns <= s.max_ns,
                    "{}",
                    r.workload
                );
            }
            other => panic!("{}: {other:?}", r.workload),
        }
    }
    let t = SpeedTable::from_rows(rows);
    assert_eq!(SpeedTable::parse(&t.to_tsv()).unwrap(), t);
}
