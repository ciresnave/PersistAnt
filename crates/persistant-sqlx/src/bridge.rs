// SPDX-License-Identifier: MIT OR Apache-2.0
//! The migration ramp (`docs/DESIGN-SQLX-BRIDGE.md` section 6, impl 2): run a statement of
//! `persistant::sqlx_query!` on an sqlx pool or connection the program already holds, so a module can
//! move to the macros one statement at a time, inside the sqlx transactions it still has.
//!
//! The translated PostgreSQL text is run with sqlx's own binds and its rows are decoded exactly as
//! [`PgBackend`](crate::PgBackend) decodes them, so a statement moves between the two with no change
//! to its call site beyond the argument of `fetch_*`. No raw connection is exposed.

use crate::{bind, bind_hinted, classify, decode};
use persistant_relational::{ArrayElem, Dialect, RelError, Row, Runner, Sql, Value, prepare};

/// An sqlx pool as a [`Runner`]: `query.fetch_all(OnPool(&pool))`.
pub struct OnPool<'p>(pub &'p sqlx::PgPool);

/// An sqlx connection as a [`Runner`], for a statement that belongs to a transaction the program
/// holds: `query.fetch_all(OnConn(&mut *tx))` (a `Transaction` dereferences to its connection).
pub struct OnConn<'c>(pub &'c mut sqlx::PgConnection);

impl Runner for OnPool<'_> {
    async fn run_query(&mut self, sql: &Sql, params: &[Value]) -> Result<Vec<Row>, RelError> {
        self.run_query_hinted(sql, params, &[]).await
    }

    async fn run_execute(&mut self, sql: &Sql, params: &[Value]) -> Result<u64, RelError> {
        self.run_execute_hinted(sql, params, &[]).await
    }

    async fn run_query_hinted(
        &mut self,
        sql: &Sql,
        params: &[Value],
        hints: &[Option<ArrayElem>],
    ) -> Result<Vec<Row>, RelError> {
        let (text, ordered) = prepare(sql, &Dialect::Postgres, params)?;
        let q = bind_hinted(
            sqlx::query(sqlx::AssertSqlSafe(text.to_string())),
            &ordered,
            hints,
        )?;
        let rows = q.fetch_all(self.0).await.map_err(classify)?;
        rows.iter().map(decode).collect()
    }

    async fn run_execute_hinted(
        &mut self,
        sql: &Sql,
        params: &[Value],
        hints: &[Option<ArrayElem>],
    ) -> Result<u64, RelError> {
        let (text, ordered) = prepare(sql, &Dialect::Postgres, params)?;
        let q = bind_hinted(
            sqlx::query(sqlx::AssertSqlSafe(text.to_string())),
            &ordered,
            hints,
        )?;
        q.execute(self.0)
            .await
            .map(|r| r.rows_affected())
            .map_err(classify)
    }
}

impl Runner for OnConn<'_> {
    async fn run_query(&mut self, sql: &Sql, params: &[Value]) -> Result<Vec<Row>, RelError> {
        self.run_query_hinted(sql, params, &[]).await
    }

    async fn run_execute(&mut self, sql: &Sql, params: &[Value]) -> Result<u64, RelError> {
        self.run_execute_hinted(sql, params, &[]).await
    }

    async fn run_query_hinted(
        &mut self,
        sql: &Sql,
        params: &[Value],
        hints: &[Option<ArrayElem>],
    ) -> Result<Vec<Row>, RelError> {
        let (text, ordered) = prepare(sql, &Dialect::Postgres, params)?;
        let q = bind_hinted(
            sqlx::query(sqlx::AssertSqlSafe(text.to_string())),
            &ordered,
            hints,
        )?;
        let rows = q.fetch_all(&mut *self.0).await.map_err(classify)?;
        rows.iter().map(decode).collect()
    }

    async fn run_execute_hinted(
        &mut self,
        sql: &Sql,
        params: &[Value],
        hints: &[Option<ArrayElem>],
    ) -> Result<u64, RelError> {
        let (text, ordered) = prepare(sql, &Dialect::Postgres, params)?;
        let q = bind_hinted(
            sqlx::query(sqlx::AssertSqlSafe(text.to_string())),
            &ordered,
            hints,
        )?;
        q.execute(&mut *self.0)
            .await
            .map(|r| r.rows_affected())
            .map_err(classify)
    }
}

/// Runtime SQL on a held sqlx pool or connection: a statement text built or written at run time, with its
/// arguments as [`Value`]s, no build-time check and no `.sqlx` data.
///
/// The text is PostgreSQL's and goes **as written** (no UniSQL translation, no cast is added): the
/// arguments bind by `$n` and the rows decode as [`PgBackend`](crate::PgBackend) decodes them, read with
/// [`Row::get`]. Because nothing describes the statement's parameter types, a `uuid`, `timestamptz`, `date`
/// or `jsonb` argument (a `Value::Text`) needs a cast in the text, and the cast must go through `text`:
/// write `CAST(CAST($1 AS text) AS uuid)` (or `$1::text::uuid`), not `$1::uuid`. A bare `$1::uuid` lets the
/// server infer the parameter as `uuid` when a `NULL` is bound first, and the connection's cached
/// statement then rejects a later text value ("incorrect binary data format").
macro_rules! runtime_sql {
    ($ty:ident, $s:ident, $conn:expr) => {
        impl $ty<'_> {
            /// Every row of `sql`.
            pub async fn query(&mut $s, sql: &str, params: &[Value]) -> Result<Vec<Row>, RelError> {
                let q = bind(sqlx::query(sqlx::AssertSqlSafe(sql.to_string())), params)?;
                let rows = q.fetch_all($conn).await.map_err(classify)?;
                rows.iter().map(decode).collect()
            }

            /// The first row, or an error if there is none (sqlx's `RowNotFound`).
            pub async fn query_one(&mut $s, sql: &str, params: &[Value]) -> Result<Row, RelError> {
                $s.query_optional(sql, params).await?.ok_or_else(|| {
                    RelError::Backend("no rows returned by a query expected to return one".into())
                })
            }

            /// The first row, or `None`.
            pub async fn query_optional(
                &mut $s,
                sql: &str,
                params: &[Value],
            ) -> Result<Option<Row>, RelError> {
                Ok($s.query(sql, params).await?.into_iter().next())
            }

            /// Run a statement that returns no rows; the number of rows affected.
            pub async fn execute(&mut $s, sql: &str, params: &[Value]) -> Result<u64, RelError> {
                let q = bind(sqlx::query(sqlx::AssertSqlSafe(sql.to_string())), params)?;
                q.execute($conn)
                    .await
                    .map(|r| r.rows_affected())
                    .map_err(classify)
            }
        }
    };
}

runtime_sql!(OnPool, self, self.0);
runtime_sql!(OnConn, self, &mut *self.0);
