// SPDX-License-Identifier: MIT OR Apache-2.0
use crate::{Dialect, RelError, Row, Value};
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

/// The isolation level a transaction asks for. A backend gives the level asked for or a stronger one;
/// which it can actually show is what the `rel.isolation.*` leaves certify.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Isolation {
    /// No dirty reads: the backend's usual level for a transaction here.
    #[default]
    ReadCommitted,
    /// A row or count read twice in the transaction reads the same.
    RepeatableRead,
    /// Concurrent transactions behave as if they ran one at a time (a conflicting one fails with
    /// [`RelError::Serialization`], or waits).
    Serializable,
}

/// A relational database, as small as it can be.
///
/// Statements are written in the backend's own [`Dialect`]; parameters are positional and written
/// as that dialect writes them (`$1` for PostgreSQL). Nothing here translates SQL.
pub trait RelationalBackend: Send + Sync {
    /// A transaction on this backend. Dropping it without `commit` rolls it back.
    type Tx<'a>: RelationalTx + 'a
    where
        Self: 'a;

    /// The backend's name as it appears in matrix rows (`postgres`).
    fn backend(&self) -> &str;

    /// The server version string as it appears in matrix rows (`16.15`).
    fn version(&self) -> &str;

    /// The dialect statements must be written in.
    fn dialect(&self) -> Dialect;

    /// Run a statement that returns no rows; the result is the number of rows affected.
    fn execute(
        &self,
        sql: &str,
        params: &[Value],
    ) -> impl Future<Output = Result<u64, RelError>> + Send;

    /// Run a statement and return its rows.
    fn query(
        &self,
        sql: &str,
        params: &[Value],
    ) -> impl Future<Output = Result<Vec<Row>, RelError>> + Send;

    /// Begin a transaction at (at least) `isolation`.
    fn begin(
        &self,
        isolation: Isolation,
    ) -> impl Future<Output = Result<Self::Tx<'_>, RelError>> + Send;

    /// The per-call timeout this backend enforces, if one is configured; `None` (the default) means a
    /// call waits as long as the server does. Every call that takes longer returns
    /// [`RelError::Timeout`].
    fn call_bound(&self) -> Option<Duration> {
        None
    }

    /// Test support for `kv.bounded_call`: take `lock_sql` (a statement that locks a row) in a
    /// transaction of its own, drive `during` while that lock is held, then roll back. `during` is
    /// dropped, unfinished, after `at_most`, so a broken bound cannot hang the test. A backend that
    /// enforces no bound returns [`RelError::Unsupported`].
    fn with_lock_held<'a>(
        &'a self,
        lock_sql: &'a str,
        at_most: Duration,
        during: Pin<Box<dyn Future<Output = ()> + Send + 'a>>,
    ) -> impl Future<Output = Result<(), RelError>> + Send {
        let _ = (lock_sql, at_most, during);
        async { Err(RelError::Unsupported("with_lock_held".to_string())) }
    }
}

/// A transaction. Statements run inside it are committed or rolled back together.
pub trait RelationalTx: Send {
    /// As [`RelationalBackend::execute`], inside the transaction.
    fn execute(
        &mut self,
        sql: &str,
        params: &[Value],
    ) -> impl Future<Output = Result<u64, RelError>> + Send;

    /// As [`RelationalBackend::query`], inside the transaction.
    fn query(
        &mut self,
        sql: &str,
        params: &[Value],
    ) -> impl Future<Output = Result<Vec<Row>, RelError>> + Send;

    /// Commit.
    fn commit(self) -> impl Future<Output = Result<(), RelError>> + Send;

    /// Roll back.
    fn rollback(self) -> impl Future<Output = Result<(), RelError>> + Send;
}
