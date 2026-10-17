// SPDX-License-Identifier: MIT OR Apache-2.0
//! `persistant_relational::sweep` against a real PostgreSQL, SQLite file, MySQL and MariaDB: the same
//! scenario on each, with the counts it must produce.
//!
//! PostgreSQL needs `PERSISTANT_TEST_DATABASE_URL`, MySQL/MariaDB `PERSISTANT_TEST_MYSQL_URL`; without
//! them these tests print a note and pass **unless** `PERSISTANT_REQUIRE_DATABASE=1` /
//! `PERSISTANT_REQUIRE_MYSQL=1` (set by the CI jobs that provide a server). SQLite needs no server.

use persistant_relational::{Dialect, RelationalBackend, SweepError, Value, sweep};
use std::sync::Arc;

fn ph(db: &impl RelationalBackend, n: usize) -> String {
    match db.dialect() {
        Dialect::Postgres => format!("${n}"),
        _ => "?".to_string(),
    }
}

async fn count(db: &impl RelationalBackend, table: &str) -> i64 {
    let rows = db
        .query(&format!("SELECT COUNT(*) FROM {table}"), &[])
        .await
        .unwrap();
    rows[0].int(0).unwrap()
}

async fn insert(db: &impl RelationalBackend, table: &str, id: i64, ts: Option<i64>) {
    let sql = format!(
        "INSERT INTO {table} (id, expires_at) VALUES ({}, {})",
        ph(db, 1),
        ph(db, 2)
    );
    let ts = ts.map_or(Value::Null, Value::Int);
    db.execute(&sql, &[Value::Int(id), ts]).await.unwrap();
}

async fn fresh(db: &impl RelationalBackend, table: &str) {
    let _ = db.execute(&format!("DROP TABLE {table}"), &[]).await;
    db.execute(
        &format!("CREATE TABLE {table} (id BIGINT PRIMARY KEY, expires_at BIGINT)"),
        &[],
    )
    .await
    .unwrap();
}

/// The scenario every backend must pass, with the counts spelled out.
async fn scenario<B: RelationalBackend + 'static>(db: Arc<B>, table: &str) {
    let cutoff = Value::Int(1000);

    // 25 expired, 5 live, 3 with no timestamp.
    fresh(&*db, table).await;
    for i in 0..25 {
        insert(&*db, table, i, Some(100 + i)).await;
    }
    for i in 25..30 {
        insert(&*db, table, i, Some(10_000 + i)).await;
    }
    for i in 30..33 {
        insert(&*db, table, i, None).await;
    }
    // Batches of 10: 10 + 10 + 5, then one that finds nothing.
    let r = sweep(&*db, table, "expires_at", &cutoff, 10, 100)
        .await
        .unwrap();
    assert_eq!((r.deleted, r.batches, r.complete), (25, 4, true), "{r:?}");
    assert_eq!(count(&*db, table).await, 8, "5 live + 3 NULL stay");
    let again = sweep(&*db, table, "expires_at", &cutoff, 10, 100)
        .await
        .unwrap();
    assert_eq!((again.deleted, again.batches, again.complete), (0, 1, true));

    // A cap stops it early, reports it, and a later call takes the rest.
    for i in 100..125 {
        insert(&*db, table, i, Some(i)).await;
    }
    let capped = sweep(&*db, table, "expires_at", &cutoff, 10, 2)
        .await
        .unwrap();
    assert_eq!(
        (capped.deleted, capped.batches, capped.complete),
        (20, 2, false)
    );
    let rest = sweep(&*db, table, "expires_at", &cutoff, 10, 100)
        .await
        .unwrap();
    assert_eq!((rest.deleted, rest.complete), (5, true));
    assert_eq!(count(&*db, table).await, 8);

    // Refused before any statement runs; the table is untouched (positive control: it is queryable).
    for bad in ["t; DROP TABLE x", "", "1t", "a b"] {
        let e = sweep(&*db, bad, "expires_at", &cutoff, 10, 1).await;
        assert!(matches!(e, Err(SweepError::BadName(_))), "{bad:?}: {e:?}");
    }
    assert!(matches!(
        sweep(&*db, table, "expires_at; --", &cutoff, 10, 1).await,
        Err(SweepError::BadName(_))
    ));
    assert_eq!(
        sweep(&*db, table, "expires_at", &Value::Null, 10, 1).await,
        Err(SweepError::NullCutoff)
    );
    assert_eq!(
        sweep(&*db, table, "expires_at", &cutoff, 0, 1).await,
        Err(SweepError::ZeroBound)
    );
    assert_eq!(count(&*db, table).await, 8);
    // A database error is reported as one (a column that does not exist).
    assert!(matches!(
        sweep(&*db, table, "no_such_column", &cutoff, 10, 1).await,
        Err(SweepError::Rel(_))
    ));

    // Concurrent writers: 200 expired rows are swept in batches of 20 while another task keeps
    // pushing the 50 live rows timestamps forward and adds more live rows. Every live row survives
    // and every expired row is gone once a final sweep has run.
    fresh(&*db, table).await;
    for i in 0..200 {
        insert(&*db, table, i, Some(i)).await;
    }
    for i in 1000..1050 {
        insert(&*db, table, i, Some(1500)).await;
    }
    let writer = {
        let db = db.clone();
        let table = table.to_string();
        tokio::spawn(async move {
            let upd = format!(
                "UPDATE {table} SET expires_at = {} WHERE id = {}",
                ph(&*db, 1),
                ph(&*db, 2)
            );
            for round in 0..4i64 {
                for i in 1000..1050i64 {
                    db.execute(&upd, &[Value::Int(2000 + round), Value::Int(i)])
                        .await
                        .unwrap();
                }
                insert(&*db, &table, 2000 + round, Some(5000)).await;
            }
        })
    };
    let mut deleted = 0;
    for _ in 0..3 {
        deleted += sweep(&*db, table, "expires_at", &cutoff, 20, 100)
            .await
            .unwrap()
            .deleted;
    }
    writer.await.unwrap();
    deleted += sweep(&*db, table, "expires_at", &cutoff, 20, 100)
        .await
        .unwrap()
        .deleted;
    assert_eq!(deleted, 200, "exactly the expired rows");
    assert_eq!(count(&*db, table).await, 54, "50 refreshed + 4 added");
    let _ = db.execute(&format!("DROP TABLE {table}"), &[]).await;
}

#[tokio::test]
async fn postgres() {
    let Ok(url) = std::env::var("PERSISTANT_TEST_DATABASE_URL") else {
        assert!(
            std::env::var("PERSISTANT_REQUIRE_DATABASE").as_deref() != Ok("1"),
            "PERSISTANT_REQUIRE_DATABASE=1 but PERSISTANT_TEST_DATABASE_URL is not set"
        );
        eprintln!("skipped: PERSISTANT_TEST_DATABASE_URL is not set");
        return;
    };
    let db = persistant_sqlx::PgBackend::connect(&url).await.unwrap();
    scenario(Arc::new(db), "persistant_sweep_t").await;
}

#[cfg(feature = "mysql")]
#[tokio::test]
async fn mysql_or_mariadb() {
    let Ok(url) = std::env::var("PERSISTANT_TEST_MYSQL_URL") else {
        assert!(
            std::env::var("PERSISTANT_REQUIRE_MYSQL").as_deref() != Ok("1"),
            "PERSISTANT_REQUIRE_MYSQL=1 but PERSISTANT_TEST_MYSQL_URL is not set"
        );
        eprintln!("skipped: PERSISTANT_TEST_MYSQL_URL is not set");
        return;
    };
    let db = persistant_sqlx::MysqlBackend::connect(&url).await.unwrap();
    let want = if std::env::var("PERSISTANT_TEST_MYSQL_FLAVOR").as_deref() == Ok("mariadb") {
        Dialect::MariaDb
    } else {
        Dialect::MySql
    };
    assert_eq!(db.dialect(), want, "the server under test is the one named");
    scenario(Arc::new(db), "persistant_sweep_t").await;
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn sqlite() {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!("persistant-sweep-{}-{n}.db", std::process::id()));
    let db = persistant_sqlx::SqliteBackend::open(&path).await.unwrap();
    scenario(Arc::new(db), "persistant_sweep_t").await;
    for ext in ["", "-wal", "-shm"] {
        let mut p = path.as_os_str().to_owned();
        p.push(ext);
        let _ = std::fs::remove_file(p);
    }
}
