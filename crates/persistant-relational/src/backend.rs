// SPDX-License-Identifier: MIT OR Apache-2.0
use crate::{Dialect, RelError, Row, Value};
use std::future::Future;

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

    /// Begin a transaction.
    fn begin(&self) -> impl Future<Output = Result<Self::Tx<'_>, RelError>> + Send;
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
