// SPDX-License-Identifier: MIT OR Apache-2.0
//! `Bounded` over SQLite (no server): a batch blocked on a real writer lock held by another
//! transaction times out, applies nothing, and `kv.bounded_call` is on with a timeout and off without.
#![cfg(feature = "sqlite")]

use persistant_core::{Config, Reason};
use persistant_kv::conformance::{Leaf, certify_leaves};
use persistant_kv::{BatchOp, KvBackend, KvError, KvOnRelational};
use persistant_relational::{Isolation, RelationalBackend, RelationalTx};
use persistant_sqlx::{Bounded, SqliteBackend};
use std::time::{Duration, Instant};

/// Generous: opening the store runs DDL through the bounded backend, and a Windows CI disk can take
/// several hundred milliseconds for that.
const BOUND: Duration = Duration::from_millis(2000);

/// Fail, rather than hang, if a call that must be cut off is not (this is how a removed timeout
/// shows up).
async fn guard<T>(call: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(20), call)
        .await
        .expect("the call hung: the bound did not fire")
}

/// A bounded store over `path`. The tables are created first through an unbounded backend: the DDL
/// of a first open can take longer than any bound that is short enough to test, on a slow CI disk
/// (the 2 s bound timed out opening on a hosted Windows runner), and what is under test is the
/// bound on a blocked call, not on setup. The second open only finds the tables there.
async fn open_bounded(path: &std::path::Path) -> KvOnRelational<Bounded<SqliteBackend>> {
    let first = SqliteBackend::open(path).await.unwrap();
    drop(KvOnRelational::open(first).await.unwrap());
    KvOnRelational::open(Bounded::new(
        SqliteBackend::open(path).await.unwrap(),
        BOUND,
    ))
    .await
    .unwrap()
}

fn temp_db(tag: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "persistant-bounded-{tag}-{}-{}.db",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

fn cleanup(path: &std::path::Path) {
    for ext in ["", "-wal", "-shm"] {
        let mut p = path.as_os_str().to_owned();
        p.push(ext);
        let _ = std::fs::remove_file(p);
    }
}

fn put(key: &str) -> Vec<BatchOp> {
    vec![BatchOp::Put {
        key: key.to_string(),
        value: b"v".to_vec(),
        ttl: None,
    }]
}

#[tokio::test]
async fn a_batch_blocked_on_a_real_lock_times_out_and_applies_nothing() {
    let path = temp_db("lock");
    let kv = open_bounded(&path).await;

    // Another transaction holds SQLite's writer lock.
    let blocker_db = SqliteBackend::open(&path).await.unwrap();
    let mut blocker = blocker_db.begin(Isolation::ReadCommitted).await.unwrap();
    blocker
        .execute("UPDATE persistant_kv_batch SET id = id WHERE id = 1", &[])
        .await
        .unwrap();

    let started = Instant::now();
    let r = guard(kv.batch(&put("blocked"))).await;
    assert!(
        matches!(r, Err(KvError::TimedOut(d)) if d == BOUND),
        "got {r:?}"
    );
    assert!(
        started.elapsed() < BOUND * 5,
        "took {:?}",
        started.elapsed()
    );

    // The control: the same batch with no bound is still waiting well past the bound (SQLite waits
    // up to its busy timeout, 30 s), so it is the bound that ended the call above.
    let plain = KvOnRelational::open(SqliteBackend::open(&path).await.unwrap_or_else(|e| {
        panic!("open: {e}");
    }));
    // Opening needs no write lock beyond what the first open already made, so it may complete; the
    // batch is what must wait.
    if let Ok(Ok(plain)) = tokio::time::timeout(BOUND * 2, plain).await {
        let waited = tokio::time::timeout(BOUND, plain.batch(&put("plain"))).await;
        assert!(waited.is_err(), "the unbounded batch returned: {waited:?}");
    }

    blocker.rollback().await.unwrap();
    // Nothing was applied, and the store works again.
    assert_eq!(kv.get("blocked").await.unwrap(), None);
    let mut ok = false;
    for _ in 0..20 {
        if matches!(kv.batch(&put("after")).await, Ok(true)) {
            ok = true;
            break;
        }
    }
    assert!(ok, "the store did not recover once the lock was gone");
    cleanup(&path);
}

#[tokio::test]
async fn bounded_call_is_on_with_a_timeout_and_off_without() {
    let leaves = [Leaf::BoundedCall];
    let none = Config::new();

    let path = temp_db("on");
    let kv = open_bounded(&path).await;
    let rows = certify_leaves(&kv, &none, &none, &leaves).await;
    assert!(rows[0].row.enabled, "{}", rows[0].row.note);
    cleanup(&path);

    // No timeout configured: the leaf is missing, not on.
    let path = temp_db("off");
    let kv = KvOnRelational::open(SqliteBackend::open(&path).await.unwrap())
        .await
        .unwrap();
    let rows = certify_leaves(&kv, &none, &none, &leaves).await;
    assert!(!rows[0].row.enabled);
    assert_eq!(rows[0].row.reason, Some(Reason::Missing));
    cleanup(&path);
}
