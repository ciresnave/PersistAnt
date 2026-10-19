// SPDX-License-Identifier: MIT OR Apache-2.0
//! `Bounded`: a per-call timeout that returns an error instead of hanging, and `kv.bounded_call`.
//!
//! The first half needs no database: a fake backend that can be told to wait forever on a statement.
//! The second half is SQLite (no server), with a real lock held by another transaction.

use persistant_core::{Config, Reason};
use persistant_kv::conformance::{Leaf, certify_leaves};
use persistant_kv::{BatchOp, KvBackend, KvError, KvOnRelational};
use persistant_relational::{
    Dialect, Isolation, RelError, RelationalBackend, RelationalTx, Row, Value,
};
use persistant_sqlx::{Bounded, SqliteBackend, parse_bound};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const BOUND: Duration = Duration::from_millis(150);

/// Fail, rather than hang, if a call that must be cut off is not (this is how a removed timeout
/// shows up).
async fn guard<T>(call: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(5), call)
        .await
        .expect("the call hung: the bound did not fire")
}

#[derive(Clone, Default)]
struct Log(Arc<Mutex<Vec<String>>>);

impl Log {
    fn push(&self, s: impl Into<String>) {
        self.0.lock().unwrap().push(s.into());
    }
    fn lines(&self) -> Vec<String> {
        self.0.lock().unwrap().clone()
    }
}

/// A backend whose statements containing `STALL` never finish.
struct Fake {
    log: Log,
}

struct FakeTx {
    log: Log,
    finished: bool,
}

impl Drop for FakeTx {
    fn drop(&mut self) {
        if !self.finished {
            self.log.push("rolled back by drop");
        }
    }
}

async fn run(log: &Log, sql: &str) -> Result<u64, RelError> {
    if sql.contains("STALL") {
        std::future::pending::<()>().await;
    }
    log.push(sql);
    Ok(1)
}

impl RelationalBackend for Fake {
    type Tx<'a> = FakeTx;
    fn backend(&self) -> &str {
        "fake"
    }
    fn version(&self) -> &str {
        "0"
    }
    fn dialect(&self) -> Dialect {
        Dialect::Postgres
    }
    async fn execute(&self, sql: &str, _: &[Value]) -> Result<u64, RelError> {
        run(&self.log, sql).await
    }
    async fn query(&self, sql: &str, _: &[Value]) -> Result<Vec<Row>, RelError> {
        run(&self.log, sql).await.map(|_| Vec::new())
    }
    async fn begin(&self, _: Isolation) -> Result<FakeTx, RelError> {
        Ok(FakeTx {
            log: self.log.clone(),
            finished: false,
        })
    }
}

impl RelationalTx for FakeTx {
    async fn execute(&mut self, sql: &str, _: &[Value]) -> Result<u64, RelError> {
        run(&self.log, sql).await
    }
    async fn query(&mut self, sql: &str, _: &[Value]) -> Result<Vec<Row>, RelError> {
        run(&self.log, sql).await.map(|_| Vec::new())
    }
    async fn commit(mut self) -> Result<(), RelError> {
        self.finished = true;
        self.log.push("commit");
        Ok(())
    }
    async fn rollback(mut self) -> Result<(), RelError> {
        self.finished = true;
        self.log.push("rollback");
        Ok(())
    }
}

#[tokio::test]
async fn a_stalled_statement_returns_the_timeout_not_a_hang() {
    let log = Log::default();
    let db = Bounded::new(Fake { log: log.clone() }, BOUND);
    assert_eq!(db.call_bound(), Some(BOUND));
    let started = Instant::now();
    let r = guard(db.execute("STALL", &[])).await;
    assert_eq!(r, Err(RelError::Timeout { after: BOUND }));
    assert!(
        started.elapsed() < BOUND * 5,
        "took {:?}",
        started.elapsed()
    );
    // A statement that does not stall is untouched.
    assert_eq!(db.execute("INSERT ok", &[]).await, Ok(1));
}

#[tokio::test]
async fn the_unbounded_control_really_does_hang() {
    // Without the wrapper the same call is still pending after five times the bound: the timeout is
    // what ends the call above, not the fake giving up.
    let db = Fake {
        log: Log::default(),
    };
    let waited = tokio::time::timeout(BOUND * 5, db.execute("STALL", &[])).await;
    assert!(waited.is_err(), "the fake returned; it was meant to hang");
    assert_eq!(RelationalBackend::call_bound(&db), None);
}

#[tokio::test]
async fn a_transaction_that_timed_out_applies_nothing() {
    let log = Log::default();
    let db = Bounded::new(Fake { log: log.clone() }, BOUND);
    let mut tx = db.begin(Isolation::ReadCommitted).await.unwrap();
    tx.execute("INSERT first", &[]).await.unwrap();
    assert_eq!(
        guard(tx.execute("STALL second", &[])).await,
        Err(RelError::Timeout { after: BOUND })
    );
    // Poisoned: nothing more runs in it, and it cannot commit.
    assert!(matches!(
        tx.execute("INSERT third", &[]).await,
        Err(RelError::Backend(_))
    ));
    assert!(matches!(tx.commit().await, Err(RelError::Backend(_))));
    let lines = log.lines();
    assert!(!lines.contains(&"commit".to_string()), "{lines:?}");
    assert!(!lines.contains(&"INSERT third".to_string()), "{lines:?}");
    assert!(
        lines.contains(&"rolled back by drop".to_string()),
        "{lines:?}"
    );
}

#[test]
fn a_bound_is_a_positive_number_of_milliseconds() {
    assert_eq!(parse_bound("1500"), Some(Duration::from_millis(1500)));
    assert_eq!(parse_bound(" 20 "), Some(Duration::from_millis(20)));
    for no in ["", "0", "-5", "soon", "1.5"] {
        assert_eq!(parse_bound(no), None, "{no:?}");
    }
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
    let kv = KvOnRelational::open(Bounded::new(
        SqliteBackend::open(&path).await.unwrap(),
        BOUND,
    ))
    .await
    .unwrap();

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
    if let Ok(Ok(plain)) = tokio::time::timeout(BOUND * 5, plain).await {
        let waited = tokio::time::timeout(BOUND * 5, plain.batch(&put("plain"))).await;
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
    let kv = KvOnRelational::open(Bounded::new(
        SqliteBackend::open(&path).await.unwrap(),
        BOUND,
    ))
    .await
    .unwrap();
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
