// SPDX-License-Identifier: MIT OR Apache-2.0
//! What `sqlx_query!`, `sqlx_query_as!` and `sqlx_query_scalar!` expand to: a statement checked and
//! translated at build time, its bound arguments and the decoder for its rows (`Bound`), and the
//! `Runner`s that execute one (`docs/DESIGN-SQLX-BRIDGE.md` sections 2 and 6). Feature `sql`.

use crate::sql_ext::{RelationalSqlExt, RelationalTxSqlExt, Sql};
use crate::{Dialect, RelError, RelationalBackend, RelationalTx, Row, Value};
use std::future::Future;

/// Something a [`Bound`] statement can run on: a backend (`&db`) or a transaction ([`InTx`]).
pub trait Runner {
    /// The rows the statement returns, with `params` in the order of its `$1..$n`.
    fn run_query<'a>(
        &'a mut self,
        sql: &'a Sql,
        params: &'a [Value],
    ) -> impl Future<Output = Result<Vec<Row>, RelError>> + Send + 'a;

    /// The number of rows the statement affected.
    fn run_execute<'a>(
        &'a mut self,
        sql: &'a Sql,
        params: &'a [Value],
    ) -> impl Future<Output = Result<u64, RelError>> + Send + 'a;
}

impl<B: RelationalBackend + ?Sized> Runner for &B {
    fn run_query<'a>(
        &'a mut self,
        sql: &'a Sql,
        params: &'a [Value],
    ) -> impl Future<Output = Result<Vec<Row>, RelError>> + Send + 'a {
        let backend: &'a B = self;
        backend.query_sql(sql, params)
    }

    fn run_execute<'a>(
        &'a mut self,
        sql: &'a Sql,
        params: &'a [Value],
    ) -> impl Future<Output = Result<u64, RelError>> + Send + 'a {
        let backend: &'a B = self;
        backend.execute_sql(sql, params)
    }
}

/// A transaction as a [`Runner`]. A transaction does not know its dialect, so it is passed in
/// (`db.dialect()`), as for the other transaction methods.
pub struct InTx<'t, T: ?Sized> {
    tx: &'t mut T,
    dialect: Dialect,
}

impl<'t, T: RelationalTx + ?Sized> InTx<'t, T> {
    /// Run on `tx`, choosing the statement text for `dialect`.
    pub fn new(tx: &'t mut T, dialect: Dialect) -> Self {
        Self { tx, dialect }
    }
}

impl<T: RelationalTx + ?Sized> Runner for InTx<'_, T> {
    fn run_query<'a>(
        &'a mut self,
        sql: &'a Sql,
        params: &'a [Value],
    ) -> impl Future<Output = Result<Vec<Row>, RelError>> + Send + 'a {
        let Self { tx, dialect } = self;
        tx.query_sql(dialect, sql, params)
    }

    fn run_execute<'a>(
        &'a mut self,
        sql: &'a Sql,
        params: &'a [Value],
    ) -> impl Future<Output = Result<u64, RelError>> + Send + 'a {
        let Self { tx, dialect } = self;
        tx.execute_sql(dialect, sql, params)
    }
}

/// A statement with its arguments bound and its rows' decoder, built by the `sqlx_query*!` macros.
/// `fetch_one`, `fetch_optional`, `fetch_all` and `execute` mirror sqlx's.
pub struct Bound<R> {
    sql: Sql,
    params: Vec<Value>,
    decode: fn(&Row) -> Result<R, RelError>,
}

impl<R> Bound<R> {
    /// For the macros: the translated statement, its arguments in `$1..$n` order, and the decoder.
    pub fn new(sql: Sql, params: Vec<Value>, decode: fn(&Row) -> Result<R, RelError>) -> Self {
        Self {
            sql,
            params,
            decode,
        }
    }

    /// Every row.
    pub async fn fetch_all<E: Runner>(self, mut on: E) -> Result<Vec<R>, RelError> {
        let rows = on.run_query(&self.sql, &self.params).await?;
        rows.iter().map(self.decode).collect()
    }

    /// The first row, or an error if there is none (sqlx's `RowNotFound`).
    pub async fn fetch_one<E: Runner>(self, on: E) -> Result<R, RelError> {
        self.fetch_optional(on).await?.ok_or_else(|| {
            RelError::Backend("no rows returned by a query that expected one".into())
        })
    }

    /// The first row, if any.
    pub async fn fetch_optional<E: Runner>(self, mut on: E) -> Result<Option<R>, RelError> {
        let rows = on.run_query(&self.sql, &self.params).await?;
        rows.first().map(self.decode).transpose()
    }

    /// Run the statement, ignoring any rows, and return the number affected.
    pub async fn execute<E: Runner>(self, mut on: E) -> Result<u64, RelError> {
        on.run_execute(&self.sql, &self.params).await
    }
}
