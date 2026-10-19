// SPDX-License-Identifier: MIT OR Apache-2.0
//! An optional per-call timeout for any [`RelationalBackend`].
//!
//! A server that cannot make progress may make a client wait without end: MySQL on a full disk
//! logs `Disk is full ... Waiting for someone to free space` and never answers (run 39325665814),
//! and any server waits on a row lock held by a stuck session. [`Bounded`] puts a clock on every
//! call: past the bound the call returns [`RelError::Timeout`] instead of hanging. The default is no
//! bound: nothing here is on unless a caller builds a [`Bounded`] (or reads a bound from the
//! environment with [`call_bound_from_env`]).
//!
//! What a timeout does and does not promise:
//! * the call returns an error naming the timeout, within about the bound;
//! * a **transaction** that saw a timeout is poisoned: every later call in it, and `commit`, return
//!   an error, and dropping it rolls it back, so a batch that timed out leaves nothing applied;
//! * a **single statement** outside a transaction, or a `commit` that timed out, may or may not have
//!   taken effect on the server: the outcome is unknown, as for any timed-out network call.

use persistant_relational::{
    Dialect, Isolation, RelError, RelationalBackend, RelationalTx, Row, Value,
};
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

/// A [`RelationalBackend`] whose every call is cut off after `bound`.
#[derive(Debug, Clone)]
pub struct Bounded<B> {
    inner: B,
    bound: Duration,
}

impl<B> Bounded<B> {
    /// Wrap `inner` so that no call waits longer than `bound`.
    pub fn new(inner: B, bound: Duration) -> Self {
        Bounded { inner, bound }
    }

    /// The wrapped backend (its calls are not bounded).
    pub fn inner(&self) -> &B {
        &self.inner
    }

    /// The per-call bound.
    pub fn bound(&self) -> Duration {
        self.bound
    }
}

/// Read a per-call bound, in milliseconds, from the environment variable `var`. `None` when it is
/// unset, empty, `0` or not a number: no bound, the default. The caller decides the variable's name.
pub fn call_bound_from_env(var: &str) -> Option<Duration> {
    parse_bound(&std::env::var(var).ok()?)
}

/// `"1500"` is 1.5 s; anything that is not a positive whole number of milliseconds is no bound.
pub fn parse_bound(text: &str) -> Option<Duration> {
    let ms: u64 = text.trim().parse().ok()?;
    (ms > 0).then(|| Duration::from_millis(ms))
}

async fn within<T>(
    bound: Duration,
    call: impl Future<Output = Result<T, RelError>>,
) -> Result<T, RelError> {
    match tokio::time::timeout(bound, call).await {
        Ok(r) => r,
        Err(_) => Err(RelError::Timeout { after: bound }),
    }
}

impl<B: RelationalBackend> RelationalBackend for Bounded<B> {
    type Tx<'a>
        = BoundedTx<B::Tx<'a>>
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
        within(self.bound, self.inner.execute(sql, params)).await
    }

    async fn query(&self, sql: &str, params: &[Value]) -> Result<Vec<Row>, RelError> {
        within(self.bound, self.inner.query(sql, params)).await
    }

    async fn begin(&self, isolation: Isolation) -> Result<Self::Tx<'_>, RelError> {
        let tx = within(self.bound, self.inner.begin(isolation)).await?;
        Ok(BoundedTx {
            inner: tx,
            bound: self.bound,
            poisoned: false,
        })
    }

    fn call_bound(&self) -> Option<Duration> {
        Some(self.bound)
    }

    async fn with_lock_held<'a>(
        &'a self,
        lock_sql: &'a str,
        at_most: Duration,
        during: Pin<Box<dyn Future<Output = ()> + Send + 'a>>,
    ) -> Result<(), RelError> {
        let mut tx = self.begin(Isolation::ReadCommitted).await?;
        // The lock is taken through the bounded transaction; nothing blocks it (the lock is free).
        RelationalTx::query(&mut tx, lock_sql, &[]).await?;
        // Drive the blocked call while the lock is held, but never longer than `at_most`: with a
        // broken bound the call would otherwise wait for a lock that this function holds.
        let _ = tokio::time::timeout(at_most, during).await;
        RelationalTx::rollback(tx).await
    }
}

/// A transaction on a [`Bounded`] backend. After any call times out it is poisoned: the statement
/// that timed out has an unknown outcome inside the transaction, so nothing more is run in it and
/// `commit` refuses; dropping it rolls it back.
#[derive(Debug)]
pub struct BoundedTx<T> {
    inner: T,
    bound: Duration,
    poisoned: bool,
}

impl<T> BoundedTx<T> {
    fn refuse(&self) -> RelError {
        RelError::Backend(
            "transaction poisoned by an earlier timeout; it can only be rolled back".to_string(),
        )
    }
}

impl<T: RelationalTx> RelationalTx for BoundedTx<T> {
    async fn execute(&mut self, sql: &str, params: &[Value]) -> Result<u64, RelError> {
        if self.poisoned {
            return Err(self.refuse());
        }
        let r = within(self.bound, self.inner.execute(sql, params)).await;
        self.poisoned |= matches!(r, Err(RelError::Timeout { .. }));
        r
    }

    async fn query(&mut self, sql: &str, params: &[Value]) -> Result<Vec<Row>, RelError> {
        if self.poisoned {
            return Err(self.refuse());
        }
        let r = within(self.bound, self.inner.query(sql, params)).await;
        self.poisoned |= matches!(r, Err(RelError::Timeout { .. }));
        r
    }

    async fn commit(self) -> Result<(), RelError> {
        if self.poisoned {
            return Err(self.refuse());
        }
        within(self.bound, self.inner.commit()).await
    }

    async fn rollback(self) -> Result<(), RelError> {
        within(self.bound, self.inner.rollback()).await
    }
}
