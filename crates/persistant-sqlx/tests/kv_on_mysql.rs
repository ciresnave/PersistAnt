// SPDX-License-Identifier: MIT OR Apache-2.0
//! The relational driver and `KvOnRelational` against a real MySQL (statements written in
//! PostgreSQL SQL and translated by UniSQL).
//!
//! Needs `PERSISTANT_TEST_MYSQL_URL`. Without it these tests print a note and pass, **unless**
//! `PERSISTANT_REQUIRE_MYSQL=1` (set by the CI job that provides a server).
#![cfg(feature = "mysql")]

use persistant_core::Config;
use persistant_kv::conformance::certify;
use persistant_kv::{KvBackend, KvOnRelational};
use persistant_relational::{Dialect, RelError, RelationalBackend, RelationalTx, Value};
use persistant_sqlx::MysqlBackend;

async fn backend() -> Option<MysqlBackend> {
    match std::env::var("PERSISTANT_TEST_MYSQL_URL") {
        Ok(url) => Some(MysqlBackend::connect(&url).await.expect("connect")),
        Err(_) => {
            assert!(
                std::env::var("PERSISTANT_REQUIRE_MYSQL").as_deref() != Ok("1"),
                "PERSISTANT_REQUIRE_MYSQL=1 but PERSISTANT_TEST_MYSQL_URL is not set"
            );
            eprintln!("skipped: PERSISTANT_TEST_MYSQL_URL is not set");
            None
        }
    }
}

#[tokio::test]
async fn statements_parameters_and_values_round_trip() {
    let Some(db) = backend().await else { return };
    assert_eq!(db.dialect(), Dialect::MySql);
    assert!(db.version().starts_with("8."), "version {:?}", db.version());
    let rows = db
        .query(
            "SELECT CAST(? AS SIGNED), CAST(? AS CHAR CHARACTER SET utf8mb4), CAST(? AS BINARY), CAST(? AS DOUBLE)",
            &[
                Value::Int(-7),
                Value::Text("ünï".into()),
                Value::Bytes(vec![0, 255, 1]),
                Value::Float(1.5),
            ],
        )
        .await
        .unwrap();
    assert_eq!(rows[0].values[0], Value::Int(-7));
    assert_eq!(rows[0].values[1], Value::Text("ünï".into()));
    assert_eq!(rows[0].values[2], Value::Bytes(vec![0, 255, 1]));
    assert_eq!(rows[0].values[3], Value::Float(1.5));
}

#[tokio::test]
async fn errors_are_classified_and_a_dropped_transaction_rolls_back() {
    let Some(db) = backend().await else { return };
    db.execute("DROP TABLE IF EXISTS persistant_t_errors", &[])
        .await
        .unwrap();
    db.execute(
        "CREATE TABLE persistant_t_errors (id BIGINT PRIMARY KEY, n BIGINT NOT NULL) ENGINE=InnoDB",
        &[],
    )
    .await
    .unwrap();
    db.execute("INSERT INTO persistant_t_errors VALUES (1, 1)", &[])
        .await
        .unwrap();
    let dup = db
        .execute("INSERT INTO persistant_t_errors VALUES (1, 2)", &[])
        .await;
    assert!(
        matches!(dup, Err(RelError::UniqueViolation { .. })),
        "{dup:?}"
    );
    {
        let mut tx = db.begin().await.unwrap();
        tx.execute("INSERT INTO persistant_t_errors VALUES (3, 3)", &[])
            .await
            .unwrap();
    }
    let n = db
        .query("SELECT COUNT(*) FROM persistant_t_errors", &[])
        .await
        .unwrap();
    assert_eq!(
        n[0].values,
        vec![Value::Int(1)],
        "a dropped transaction must roll back"
    );
    db.execute("DROP TABLE persistant_t_errors", &[])
        .await
        .unwrap();
}

#[tokio::test]
async fn kv_on_mysql_certifies_every_leaf_on() {
    let Some(db) = backend().await else { return };
    let kv = KvOnRelational::open(db).await.expect("open");
    kv.put("a", b"1".to_vec(), None).await.unwrap();
    assert_eq!(kv.get("a").await.unwrap().as_deref(), Some(&b"1"[..]));
    let config = Config::new().with("dialect", "mysql");
    let rows = certify(&kv, &config, &Config::new()).await;
    let off: Vec<_> = rows
        .iter()
        .filter(|c| !c.row.enabled)
        .map(|c| format!("{}: {:?} {}", c.leaf.id(), c.row.reason, c.row.note))
        .collect();
    assert!(off.is_empty(), "leaves off on MySQL: {off:#?}");
}

/// The fact `KvOnRelational::claim_two_step` is written around. If sqlx ever stops setting
/// `CLIENT_FOUND_ROWS` this fails, and the two-step can go back to one statement.
#[tokio::test]
async fn an_upsert_cannot_say_whether_it_stored() {
    let Some(db) = backend().await else { return };
    db.execute("DROP TABLE IF EXISTS persistant_t_upsert", &[])
        .await
        .unwrap();
    db.execute(
        "CREATE TABLE persistant_t_upsert (k BIGINT PRIMARY KEY, v BIGINT NOT NULL) ENGINE=InnoDB",
        &[],
    )
    .await
    .unwrap();
    let up = |k: i64, v: i64, set: &'static str| {
        let db = &db;
        async move {
            db.execute(
                &format!("INSERT INTO persistant_t_upsert VALUES (?, ?) ON DUPLICATE KEY UPDATE v = {set}"),
                &[Value::Int(k), Value::Int(v)],
            )
            .await
            .unwrap()
        }
    };
    assert_eq!(up(1, 1, "v").await, 1, "insert");
    assert_eq!(
        up(1, 1, "v").await,
        1,
        "existing row, nothing changed: 1 under CLIENT_FOUND_ROWS, 0 without"
    );
    assert_eq!(up(1, 2, "5").await, 2, "existing row, changed");
    db.execute("DROP TABLE persistant_t_upsert", &[])
        .await
        .unwrap();
}

/// Stage 2: the sampled-concurrency leaves, one attempt per run, five runs. One wrong run in five
/// is a flaky contract, not a pass. (Real-time leaves are left out: a stalled CI machine would fail
/// them for the wrong reason; `certify` retries those.)
#[tokio::test]
async fn the_concurrency_leaves_are_exact_on_every_run() {
    let Some(db) = backend().await else { return };
    use persistant_kv::conformance::Leaf;
    use persistant_kv::stats::measure;
    let kv = KvOnRelational::open(db).await.expect("open");
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
