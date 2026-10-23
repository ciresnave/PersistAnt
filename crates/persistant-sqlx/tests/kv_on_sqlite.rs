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
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "persistant-{tag}-{}-{n}-{seq}.db",
        std::process::id()
    ))
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
        let mut tx = db
            .begin(persistant_relational::Isolation::ReadCommitted)
            .await
            .unwrap();
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
    // kv.bounded_call is off here by design: no call timeout is configured on this backend.
    let off: Vec<_> = rows
        .iter()
        .filter(|c| !c.row.enabled && c.leaf != persistant_kv::conformance::Leaf::BoundedCall)
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
        Leaf::Take,
        Leaf::Index,
        Leaf::BatchRollback,
        Leaf::BatchConcurrentRead,
        Leaf::OptimisticSequence,
        Leaf::AbsentRace,
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

/// The relational contract (`persistant_relational::conformance`) on SQLite: every leaf on, with its
/// known-bad controls (a backend with no transactions, lossy bytes, unclassified errors, and the
/// schema without the constraint) caught.
#[tokio::test]
async fn the_relational_leaves_certify_on() {
    let (db, path) = open("rel").await;
    let config = Config::new().with("dialect", "sqlite");
    let rows = persistant_relational::conformance::certify(&db, &config, &Config::new()).await;
    let off: Vec<_> = rows
        .iter()
        .filter(|c| !c.row.enabled)
        .map(|c| format!("{}: {:?} {}", c.leaf.id(), c.row.reason, c.row.note))
        .collect();
    // SQLite has no row locks (a write locks the whole database) and its `begin` holds that lock, so the
    // probes that need a concurrent commit or transaction cannot run: those three leaves are honestly off.
    let ids: Vec<&str> = off.iter().map(|o| o.split(':').next().unwrap()).collect();
    assert_eq!(
        ids,
        [
            "rel.isolation.repeatable_read",
            "rel.isolation.serializable",
            "rel.row_lock"
        ],
        "{off:#?}"
    );
    assert!(off.iter().all(|o| o.contains("Some(Missing)")), "{off:#?}");
    assert_eq!(
        rows.len(),
        persistant_relational::conformance::Leaf::ALL.len()
    );
    cleanup(&path);
}

/// The migration ledger (`persistant_relational::migrate_conformance`) on SQLite, in a fresh file: every
/// leaf on, with the ledger-less migrator caught.
#[tokio::test]
async fn the_migration_leaves_certify_on() {
    let (db, path) = open("migrations").await;
    let config = Config::new().with("dialect", "sqlite");
    let rows =
        persistant_relational::migrate_conformance::certify(&db, &config, &Config::new()).await;
    let off: Vec<_> = rows
        .iter()
        .filter(|c| !c.row.enabled)
        .map(|c| format!("{}: {:?} {}", c.leaf.id(), c.row.reason, c.row.note))
        .collect();
    assert!(off.is_empty(), "migration leaves off on SQLite: {off:#?}");
    assert_eq!(
        rows.len(),
        persistant_relational::migrate_conformance::MigrateLeaf::ALL.len()
    );
    drop(db);
    cleanup(&path);
}

/// The `synchronous=FULL` profile is set on every pooled connection, carries its own name, and is the
/// only SQLite profile the kv `choose` table declares `Machine` for. The default `open` is not.
#[tokio::test]
async fn open_durable_sets_full_sync_and_declares_machine() {
    use persistant_kv::choose::{Durability, declared_durability};
    let path = fresh_path("durable");
    let db = SqliteBackend::open_durable(&path).await.expect("open");
    // Hold several connections at once so the check covers more than the first one.
    let mut conns = Vec::new();
    for _ in 0..4 {
        conns.push(db.pool().acquire().await.expect("acquire"));
    }
    for c in &mut conns {
        let sync: i64 = sqlx::query_scalar("PRAGMA synchronous")
            .fetch_one(&mut **c)
            .await
            .unwrap();
        assert_eq!(sync, 2, "PRAGMA synchronous: 2 is FULL");
        let mode: String = sqlx::query_scalar("PRAGMA journal_mode")
            .fetch_one(&mut **c)
            .await
            .unwrap();
        assert_eq!(mode, "wal");
    }
    drop(conns);
    assert_eq!(RelationalBackend::backend(&db), "sqlite-full");
    let kv = KvOnRelational::open(db).await.expect("open");
    assert_eq!(kv.backend(), "kv-on-relational:sqlite-full");
    let d = declared_durability(kv.backend()).expect("known");
    assert_eq!(d.class, Durability::Machine);
    assert_eq!(d.verified, Some(Durability::Machine));
    let rows = certify(
        &kv,
        &Config::new().with("dialect", "sqlite"),
        &Config::new(),
    )
    .await;
    assert!(rows.iter().all(|c| c.row.reason != Some(Reason::Wrong)));

    // The control: the default open keeps its name and its class.
    let (plain, plain_path) = open("plain").await;
    assert_eq!(RelationalBackend::backend(&plain), "sqlite");
    let d = declared_durability("kv-on-relational:sqlite").expect("known");
    assert_eq!(d.class, Durability::Process);
    cleanup(&path);
    cleanup(&plain_path);
}
