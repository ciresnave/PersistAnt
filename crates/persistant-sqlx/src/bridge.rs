// SPDX-License-Identifier: MIT OR Apache-2.0
//! The migration ramp (`docs/DESIGN-SQLX-BRIDGE.md` section 6, impl 2): run a statement of
//! `persistant::sqlx_query!` on an sqlx pool or connection the program already holds, so a module can
//! move to the macros one statement at a time, inside the sqlx transactions it still has.
//!
//! The translated PostgreSQL text is run with sqlx's own binds and its rows are decoded exactly as
//! [`PgBackend`](crate::PgBackend) decodes them, so a statement moves between the two with no change
//! to its call site beyond the argument of `fetch_*`. No raw connection is exposed.

use crate::{bind, classify, decode};
use persistant_relational::{Dialect, RelError, Row, Runner, Sql, Value, prepare};

/// An sqlx pool as a [`Runner`]: `query.fetch_all(OnPool(&pool))`.
pub struct OnPool<'p>(pub &'p sqlx::PgPool);

/// An sqlx connection as a [`Runner`], for a statement that belongs to a transaction the program
/// holds: `query.fetch_all(OnConn(&mut *tx))` (a `Transaction` dereferences to its connection).
pub struct OnConn<'c>(pub &'c mut sqlx::PgConnection);

impl Runner for OnPool<'_> {
    async fn run_query(&mut self, sql: &Sql, params: &[Value]) -> Result<Vec<Row>, RelError> {
        let (text, ordered) = prepare(sql, &Dialect::Postgres, params)?;
        let q = bind(sqlx::query(sqlx::AssertSqlSafe(text.to_string())), &ordered)?;
        let rows = q.fetch_all(self.0).await.map_err(classify)?;
        rows.iter().map(decode).collect()
    }

    async fn run_execute(&mut self, sql: &Sql, params: &[Value]) -> Result<u64, RelError> {
        let (text, ordered) = prepare(sql, &Dialect::Postgres, params)?;
        let q = bind(sqlx::query(sqlx::AssertSqlSafe(text.to_string())), &ordered)?;
        q.execute(self.0)
            .await
            .map(|r| r.rows_affected())
            .map_err(classify)
    }
}

impl Runner for OnConn<'_> {
    async fn run_query(&mut self, sql: &Sql, params: &[Value]) -> Result<Vec<Row>, RelError> {
        let (text, ordered) = prepare(sql, &Dialect::Postgres, params)?;
        let q = bind(sqlx::query(sqlx::AssertSqlSafe(text.to_string())), &ordered)?;
        let rows = q.fetch_all(&mut *self.0).await.map_err(classify)?;
        rows.iter().map(decode).collect()
    }

    async fn run_execute(&mut self, sql: &Sql, params: &[Value]) -> Result<u64, RelError> {
        let (text, ordered) = prepare(sql, &Dialect::Postgres, params)?;
        let q = bind(sqlx::query(sqlx::AssertSqlSafe(text.to_string())), &ordered)?;
        q.execute(&mut *self.0)
            .await
            .map(|r| r.rows_affected())
            .map_err(classify)
    }
}
