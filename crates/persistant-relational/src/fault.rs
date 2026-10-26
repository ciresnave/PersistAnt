// SPDX-License-Identifier: MIT OR Apache-2.0
//! Fault injection for the relational kind (`docs/DESIGN-KINDS.md` section 5, audit row 7): a
//! [`Faulty`] wrapper that makes chosen calls of any [`RelationalBackend`] fail at the statement
//! and commit boundary, so a consumer can test what it does when its database misbehaves.
//!
//! The same three controls as `persistant_kv::Faulty`:
//!
//! - [`Faulty::fail_nth`]: the Nth call fails **before** it runs; nothing happened.
//! - [`Faulty::interrupt_nth`]: the Nth call **runs, then reports an error**. On `tx.commit` this is
//!   the dangerous case a consumer must survive: the transaction is committed and the caller is told
//!   it failed.
//! - [`Faulty::mask`]: an operation answers [`RelError::Unsupported`].
//!
//! Operations are `execute`, `query`, `begin` and, inside a transaction, `tx.execute`, `tx.query`,
//! `tx.commit`, `tx.rollback`. "The Nth" counts calls of one operation (`Some("tx.commit")`) or of
//! every operation (`None`), from 1, across the backend and every transaction it began. A rule fires
//! once. A failed `begin` returns no transaction. This is a wrapper, not a fake database: the
//! relational harness stays a real ephemeral database.

use crate::{Dialect, Isolation, RelError, RelationalBackend, RelationalTx, Row, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Debug)]
struct Rule {
    op: Option<&'static str>,
    nth: u64,
    error: RelError,
    /// Run the call first and drop its result.
    after_effect: bool,
}

#[derive(Debug, Default)]
struct State {
    total: u64,
    per_op: BTreeMap<&'static str, u64>,
    rules: Vec<Rule>,
    masked: BTreeSet<&'static str>,
}

impl State {
    /// Count the call and decide its fate: `Ok(None)` run it, `Ok(Some(rule))` a fired rule,
    /// `Err` masked.
    fn decide(shared: &Mutex<State>, op: &'static str) -> Result<Option<Rule>, RelError> {
        let Ok(mut s) = shared.lock() else {
            return Ok(None);
        };
        s.total += 1;
        let total = s.total;
        let n = {
            let c = s.per_op.entry(op).or_default();
            *c += 1;
            *c
        };
        if s.masked.contains(op) {
            return Err(RelError::Unsupported(op.to_string()));
        }
        let hit = s.rules.iter().position(|r| match r.op {
            Some(o) => o == op && r.nth == n,
            None => r.nth == total,
        });
        Ok(hit.map(|i| s.rules.remove(i)))
    }
}

async fn run<T>(
    shared: &Mutex<State>,
    op: &'static str,
    call: impl Future<Output = Result<T, RelError>>,
) -> Result<T, RelError> {
    match State::decide(shared, op)? {
        None => call.await,
        Some(rule) => {
            if rule.after_effect {
                let _ = call.await;
            }
            Err(rule.error)
        }
    }
}

/// A [`RelationalBackend`] that fails, interrupts or masks chosen calls of the wrapped backend and
/// of the transactions it begins.
#[derive(Debug)]
pub struct Faulty<R> {
    inner: R,
    state: Arc<Mutex<State>>,
}

/// A transaction begun through [`Faulty`]; it shares the wrapper's rules and counters.
#[derive(Debug)]
pub struct FaultyTx<T> {
    inner: T,
    state: Arc<Mutex<State>>,
}

impl<R: RelationalBackend> Faulty<R> {
    /// Wrap `inner`; with no rules it behaves as `inner` does.
    pub fn new(inner: R) -> Self {
        Faulty {
            inner,
            state: Arc::new(Mutex::new(State::default())),
        }
    }

    /// The wrapped backend.
    pub fn into_inner(self) -> R {
        self.inner
    }

    /// The Nth call of `op` (or of any operation, for `None`) returns `error` without running.
    #[must_use]
    pub fn fail_nth(self, op: Option<&'static str>, nth: u64, error: RelError) -> Self {
        self.rule(op, nth, error, false)
    }

    /// The Nth call of `op` (or of any operation, for `None`) runs, then returns `error`: whatever
    /// it did stays done.
    #[must_use]
    pub fn interrupt_nth(self, op: Option<&'static str>, nth: u64, error: RelError) -> Self {
        self.rule(op, nth, error, true)
    }

    /// Every call of `op` returns [`RelError::Unsupported`] without running.
    #[must_use]
    pub fn mask(self, op: &'static str) -> Self {
        if let Ok(mut s) = self.state.lock() {
            s.masked.insert(op);
        }
        self
    }

    /// How many calls of `op` have been made, counting those that were failed or masked.
    pub fn calls(&self, op: &'static str) -> u64 {
        self.state
            .lock()
            .map(|s| s.per_op.get(op).copied().unwrap_or(0))
            .unwrap_or(0)
    }

    fn rule(self, op: Option<&'static str>, nth: u64, error: RelError, after_effect: bool) -> Self {
        if let Ok(mut s) = self.state.lock() {
            s.rules.push(Rule {
                op,
                nth,
                error,
                after_effect,
            });
        }
        self
    }
}

impl<R: RelationalBackend> RelationalBackend for Faulty<R> {
    type Tx<'a>
        = FaultyTx<R::Tx<'a>>
    where
        Self: 'a;

    fn backend(&self) -> &str {
        self.inner.backend()
    }

    fn version(&self) -> &str {
        self.inner.version()
    }

    fn dialect(&self) -> Dialect {
        self.inner.dialect()
    }

    async fn execute(&self, sql: &str, params: &[Value]) -> Result<u64, RelError> {
        run(&self.state, "execute", self.inner.execute(sql, params)).await
    }

    async fn query(&self, sql: &str, params: &[Value]) -> Result<Vec<Row>, RelError> {
        run(&self.state, "query", self.inner.query(sql, params)).await
    }

    async fn begin(&self, isolation: Isolation) -> Result<Self::Tx<'_>, RelError> {
        let inner = run(&self.state, "begin", self.inner.begin(isolation)).await?;
        Ok(FaultyTx {
            inner,
            state: self.state.clone(),
        })
    }

    fn call_bound(&self) -> Option<Duration> {
        self.inner.call_bound()
    }

    fn with_lock_held<'a>(
        &'a self,
        lock_sql: &'a str,
        at_most: Duration,
        during: Pin<Box<dyn Future<Output = ()> + Send + 'a>>,
    ) -> impl Future<Output = Result<(), RelError>> + Send {
        self.inner.with_lock_held(lock_sql, at_most, during)
    }
}

impl<T: RelationalTx> RelationalTx for FaultyTx<T> {
    async fn execute(&mut self, sql: &str, params: &[Value]) -> Result<u64, RelError> {
        run(&self.state, "tx.execute", self.inner.execute(sql, params)).await
    }

    async fn query(&mut self, sql: &str, params: &[Value]) -> Result<Vec<Row>, RelError> {
        run(&self.state, "tx.query", self.inner.query(sql, params)).await
    }

    async fn commit(self) -> Result<(), RelError> {
        run(&self.state, "tx.commit", self.inner.commit()).await
    }

    async fn rollback(self) -> Result<(), RelError> {
        run(&self.state, "tx.rollback", self.inner.rollback()).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// A backend with no database: it counts statements and commits it actually ran.
    #[derive(Default)]
    struct Fake {
        executed: AtomicU64,
        committed: Arc<AtomicU64>,
    }

    struct FakeTx {
        committed: Arc<AtomicU64>,
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
        async fn execute(&self, _: &str, _: &[Value]) -> Result<u64, RelError> {
            self.executed.fetch_add(1, Ordering::SeqCst);
            Ok(1)
        }
        async fn query(&self, _: &str, _: &[Value]) -> Result<Vec<Row>, RelError> {
            Ok(vec![])
        }
        async fn begin(&self, _: Isolation) -> Result<FakeTx, RelError> {
            Ok(FakeTx {
                committed: self.committed.clone(),
            })
        }
    }

    impl RelationalTx for FakeTx {
        async fn execute(&mut self, _: &str, _: &[Value]) -> Result<u64, RelError> {
            Ok(1)
        }
        async fn query(&mut self, _: &str, _: &[Value]) -> Result<Vec<Row>, RelError> {
            Ok(vec![])
        }
        async fn commit(self) -> Result<(), RelError> {
            self.committed.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        async fn rollback(self) -> Result<(), RelError> {
            Ok(())
        }
    }

    fn boom() -> RelError {
        RelError::Backend("boom".to_string())
    }

    #[tokio::test]
    async fn the_nth_statement_fails_before_it_runs_and_only_once() {
        let db = Faulty::new(Fake::default()).fail_nth(Some("execute"), 2, boom());
        db.execute("x", &[]).await.unwrap();
        assert_eq!(db.execute("x", &[]).await, Err(boom()));
        db.execute("x", &[]).await.unwrap();
        // Two ran; the failed one never reached the database.
        assert_eq!(db.into_inner().executed.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn an_interrupted_statement_runs_and_then_reports_the_error() {
        let db = Faulty::new(Fake::default()).interrupt_nth(Some("execute"), 1, boom());
        assert_eq!(db.execute("x", &[]).await, Err(boom()));
        assert_eq!(db.into_inner().executed.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn an_interrupted_commit_commits_and_tells_the_caller_it_failed() {
        let fake = Fake::default();
        let committed = fake.committed.clone();
        let db = Faulty::new(fake).interrupt_nth(Some("tx.commit"), 1, boom());
        let tx = db.begin(Isolation::default()).await.unwrap();
        assert_eq!(tx.commit().await, Err(boom()));
        assert_eq!(committed.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_failed_commit_does_not_commit() {
        let fake = Fake::default();
        let committed = fake.committed.clone();
        let db = Faulty::new(fake).fail_nth(Some("tx.commit"), 1, boom());
        let tx = db.begin(Isolation::default()).await.unwrap();
        assert_eq!(tx.commit().await, Err(boom()));
        assert_eq!(committed.load(Ordering::SeqCst), 0);
        // Control: the next transaction commits.
        db.begin(Isolation::default())
            .await
            .unwrap()
            .commit()
            .await
            .unwrap();
        assert_eq!(committed.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn counts_span_the_backend_and_its_transactions() {
        let db = Faulty::new(Fake::default()).fail_nth(None, 3, boom());
        db.execute("x", &[]).await.unwrap();
        let mut tx = db.begin(Isolation::default()).await.unwrap();
        assert_eq!(tx.execute("x", &[]).await, Err(boom()));
        tx.rollback().await.unwrap();
        assert_eq!(db.calls("tx.execute"), 1);
        assert_eq!(db.calls("tx.rollback"), 1);
    }

    #[tokio::test]
    async fn a_failed_begin_returns_no_transaction_and_a_masked_op_is_unsupported() {
        let db = Faulty::new(Fake::default())
            .fail_nth(Some("begin"), 1, RelError::Unavailable("gone".to_string()))
            .mask("query");
        assert!(matches!(
            db.begin(Isolation::default()).await,
            Err(RelError::Unavailable(_))
        ));
        assert_eq!(
            db.query("x", &[]).await,
            Err(RelError::Unsupported("query".to_string()))
        );
        // Control: the unwrapped backend answers the same query.
        assert_eq!(Fake::default().query("x", &[]).await, Ok(vec![]));
        db.execute("x", &[]).await.unwrap();
    }
}
