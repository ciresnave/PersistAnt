// SPDX-License-Identifier: MIT OR Apache-2.0
//! `Bounded`: a per-call timeout that returns an error instead of hanging, and `kv.bounded_call`.
//!
//! No database: a fake backend that can be told to wait forever on a statement. The same leaf on a
//! real SQLite lock is in `bounded_sqlite.rs`.

use persistant_relational::{
    Dialect, Isolation, RelError, RelationalBackend, RelationalTx, Row, Value,
};
use persistant_sqlx::{Bounded, parse_bound};
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
