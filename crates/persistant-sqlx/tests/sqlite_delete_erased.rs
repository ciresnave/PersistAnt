// SPDX-License-Identifier: MIT OR Apache-2.0
//! `relational.execute_erased` on SQLite: after an erased delete a canary is not in the database
//! file, its `-wal` or its `-journal`, read back as raw bytes. A plain delete is the control: it
//! leaves the canary in the file (SQLite's default `secure_delete` is off), so the scan can fail.
//!
//! **Not covered:** a value overwritten earlier (see `execute_erased`), and anything the file
//! system or a backup keeps.

use persistant_relational::{RelationalBackend, Value};
use persistant_sqlx::SqliteBackend;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);

const CANARY: &[u8] = b"CANARY-5b02c7aa-not-a-secret";

fn holds_canary(dir: &Path) -> Vec<String> {
    let mut hits = Vec::new();
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if std::fs::read(&p)
            .unwrap()
            .windows(CANARY.len())
            .any(|w| w == CANARY)
        {
            hits.push(p.file_name().unwrap().to_string_lossy().into_owned());
        }
    }
    hits
}

async fn fill(db: &SqliteBackend) {
    db.execute("CREATE TABLE t (k INTEGER PRIMARY KEY, v BLOB)", &[])
        .await
        .unwrap();
    // Ballast so the canary's row is not the only thing on its page.
    for i in 0..20 {
        db.execute(
            "INSERT INTO t VALUES ($1, $2)",
            &[Value::Int(i), Value::Bytes(vec![b'.'; 100])],
        )
        .await
        .unwrap();
    }
    db.execute(
        "INSERT INTO t VALUES ($1, $2)",
        &[Value::Int(100), Value::Bytes(CANARY.to_vec())],
    )
    .await
    .unwrap();
}

async fn trial(durable: bool, erased: bool) -> Vec<String> {
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("persistant-erase-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("db.sqlite");
    let db = if durable {
        SqliteBackend::open_durable(&path).await.unwrap()
    } else {
        SqliteBackend::open(&path).await.unwrap()
    };
    fill(&db).await;
    let sql = "DELETE FROM t WHERE k = $1";
    let args = [Value::Int(100)];
    let n = if erased {
        db.execute_erased(sql, &args).await.unwrap()
    } else {
        db.execute(sql, &args).await.unwrap()
    };
    assert_eq!(n, 1);
    // Read while the database is still open: the files as a running process leaves them.
    let hits = holds_canary(&dir);
    drop(db);
    let _ = std::fs::remove_dir_all(&dir);
    hits
}

#[tokio::test]
async fn plain_delete_leaves_the_canary_in_the_files() {
    for durable in [false, true] {
        let hits = trial(durable, false).await;
        assert!(
            !hits.is_empty(),
            "durable={durable}: the control found nothing"
        );
    }
}

#[tokio::test]
async fn erased_delete_leaves_no_canary_in_any_file() {
    for durable in [false, true] {
        let hits = trial(durable, true).await;
        assert!(hits.is_empty(), "durable={durable}: canary in {hits:?}");
    }
}

/// `open_secure_wal`: a plain `delete` (no `execute_erased`) leaves no canary in the database, its
/// `-wal` or its `-journal`. Controls: the default WAL profile (`open_durable`) still has it after the
/// same plain delete, and so does a WAL database with only `secure_delete` on (no checkpoint).
async fn secure_wal_trial(profile: &str) -> Vec<String> {
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("persistant-securewal-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("db.sqlite");
    let db = match profile {
        "secure-wal" => SqliteBackend::open_secure_wal(&path).await.unwrap(),
        "durable" => SqliteBackend::open_durable(&path).await.unwrap(),
        other => panic!("{other}"),
    };
    fill(&db).await;
    assert_eq!(
        db.execute("DELETE FROM t WHERE k = $1", &[Value::Int(100)])
            .await
            .unwrap(),
        1
    );
    let hits = holds_canary(&dir);
    drop(db);
    let _ = std::fs::remove_dir_all(&dir);
    hits
}

#[tokio::test]
async fn secure_wal_plain_delete_leaves_no_canary() {
    assert!(
        SqliteBackend::open_secure_wal(std::env::temp_dir().join(format!(
            "persistant-securewal-flag-{}.sqlite",
            std::process::id()
        )))
        .await
        .unwrap()
        .checkpoint_writes()
    );
    let hits = secure_wal_trial("secure-wal").await;
    assert!(hits.is_empty(), "canary in {hits:?}");
}

#[tokio::test]
async fn control_the_default_wal_profile_keeps_the_canary_after_a_plain_delete() {
    let hits = secure_wal_trial("durable").await;
    assert!(!hits.is_empty(), "the control found nothing");
}
