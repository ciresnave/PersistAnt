// SPDX-License-Identifier: MIT OR Apache-2.0
//! `COPY ... TO STDOUT` and `COPY ... FROM STDIN` on a [`PgTx`] (`docs/DESIGN-PG-NATIVE-SESSION.md`,
//! phase 3).
//!
//! Rules, and why: only `STDOUT` / `STDIN` are ever emitted (server-side file and `PROGRAM` forms run
//! as the database server's user, never as the caller); the format is the closed [`CopyFormat`] with
//! PostgreSQL's own defaults (no free `WITH (..)` text, so the NULL marker and delimiters are the
//! documented ones for that format); table and column names are [`Ident`]s; a query source is a
//! `&'static str` template with identifier holes, like [`PgTx::query_ident`]. A COPY borrows the
//! transaction mutably, so nothing else runs on it until the COPY ends.

use super::{Ident, IdentArg, render};
use crate::{PgTx, classify};
use bytes::Bytes;
use futures_core::Stream;
use persistant_relational::RelError;
use std::pin::Pin;
use std::task::{Context, Poll};

/// The data format of a COPY, with PostgreSQL's defaults for it: `Text` (tab-separated, NULL as `\N`),
/// `Csv` (comma-separated, NULL as an empty unquoted field), `Binary`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CopyFormat {
    /// `FORMAT text`.
    #[default]
    Text,
    /// `FORMAT csv`.
    Csv,
    /// `FORMAT binary`.
    Binary,
}

impl CopyFormat {
    fn as_sql(self) -> &'static str {
        match self {
            CopyFormat::Text => "text",
            CopyFormat::Csv => "csv",
            CopyFormat::Binary => "binary",
        }
    }
}

fn column_list(columns: &[Ident]) -> String {
    if columns.is_empty() {
        String::new()
    } else {
        let c: Vec<&str> = columns.iter().map(Ident::as_sql).collect();
        format!(" ({})", c.join(", "))
    }
}

/// What a COPY OUT reads: a table (optionally some columns, in the order given) or a `SELECT`.
#[derive(Debug, Clone)]
pub struct CopyOut {
    source: String,
    format: CopyFormat,
}

impl CopyOut {
    /// The whole table, or with [`CopyOut::table_columns`] the listed columns in that order.
    pub fn table(table: &Ident) -> Self {
        Self::table_columns(table, &[])
    }

    /// `COPY table (cols) TO STDOUT`: an empty `columns` is every column.
    pub fn table_columns(table: &Ident, columns: &[Ident]) -> Self {
        CopyOut {
            source: format!("{}{}", table.as_sql(), column_list(columns)),
            format: CopyFormat::default(),
        }
    }

    /// `COPY (query) TO STDOUT`. `template` is a `&'static str` with `{n}` identifier holes (as
    /// [`PgTx::query_ident`]) and must be a `SELECT`, `WITH` or `VALUES` statement; it takes no
    /// parameters (PostgreSQL does not allow them in a COPY).
    pub fn query(template: &'static str, idents: &[IdentArg<'_>]) -> Result<Self, RelError> {
        let first = template
            .trim_start()
            .split(|c: char| !c.is_ascii_alphabetic())
            .next()
            .unwrap_or("")
            .to_ascii_uppercase();
        if !matches!(first.as_str(), "SELECT" | "WITH" | "VALUES") {
            return Err(RelError::Unsupported(
                "a COPY query must start with SELECT, WITH or VALUES".into(),
            ));
        }
        let sql = render(template, idents)?;
        Ok(CopyOut {
            source: format!("({})", sql.trim().trim_end_matches(';')),
            format: CopyFormat::default(),
        })
    }

    /// The data format (default `Text`).
    pub fn format(mut self, format: CopyFormat) -> Self {
        self.format = format;
        self
    }

    pub(crate) fn statement(&self) -> String {
        format!(
            "COPY {} TO STDOUT WITH (FORMAT {})",
            self.source,
            self.format.as_sql()
        )
    }
}

/// Where a COPY IN writes: a table, and optionally the columns the data carries, in order.
#[derive(Debug, Clone)]
pub struct CopyIn {
    target: String,
    format: CopyFormat,
}

impl CopyIn {
    /// Every column of the table.
    pub fn table(table: &Ident) -> Self {
        Self::table_columns(table, &[])
    }

    /// `COPY table (cols) FROM STDIN`.
    pub fn table_columns(table: &Ident, columns: &[Ident]) -> Self {
        CopyIn {
            target: format!("{}{}", table.as_sql(), column_list(columns)),
            format: CopyFormat::default(),
        }
    }

    /// The data format (default `Text`).
    pub fn format(mut self, format: CopyFormat) -> Self {
        self.format = format;
        self
    }

    pub(crate) fn statement(&self) -> String {
        format!(
            "COPY {} FROM STDIN WITH (FORMAT {})",
            self.target,
            self.format.as_sql()
        )
    }
}

/// The data of a COPY OUT, in chunks of the chosen format. It borrows the transaction, so nothing
/// else runs on it until the stream is gone.
///
/// **Read it to the end, or call [`CopyOutStream::drain`].** Dropping it with data unread is allowed
/// but leaves the connection mid-COPY: the next statement on that transaction fails (measured:
/// `unexpected message: CopyData`), `rollback` succeeds, and a pooled connection is healthy after
/// that.
pub struct CopyOutStream<'c> {
    inner: futures_core::stream::BoxStream<'c, Result<Bytes, sqlx::Error>>,
}

impl CopyOutStream<'_> {
    /// Read and discard the rest of the data, so the transaction is usable again.
    pub async fn drain(mut self) -> Result<(), RelError> {
        while let Some(chunk) = std::future::poll_fn(|cx| Pin::new(&mut self).poll_next(cx)).await {
            chunk?;
        }
        Ok(())
    }
}

impl Stream for CopyOutStream<'_> {
    type Item = Result<Bytes, RelError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.inner
            .as_mut()
            .poll_next(cx)
            .map(|o| o.map(|r| r.map_err(classify)))
    }
}

/// A COPY IN in progress. [`CopyInSink::finish`] adds the COPY's rows to the transaction; dropping the
/// sink, or [`CopyInSink::abort`], fails the COPY: the server aborts the transaction, so its next
/// statement fails (with the COPY's failure) and only a rollback is left. A half-written COPY
/// therefore cannot be committed by accident. Do not `commit` after it: roll back (the rollback's own
/// result may report the abort) or drop the transaction.
pub struct CopyInSink<'c> {
    inner: sqlx::postgres::PgCopyIn<&'c mut sqlx::PgConnection>,
}

impl CopyInSink<'_> {
    /// Send a chunk of data. Chunks need not end on a row boundary.
    pub async fn send(&mut self, data: impl AsRef<[u8]>) -> Result<(), RelError> {
        self.inner
            .send(data.as_ref())
            .await
            .map(|_| ())
            .map_err(classify)
    }

    /// End the COPY and return the number of rows it inserted.
    pub async fn finish(self) -> Result<u64, RelError> {
        self.inner.finish().await.map_err(classify)
    }

    /// Fail the COPY with `reason`; its rows are discarded and the transaction is aborted.
    pub async fn abort(self, reason: &str) -> Result<(), RelError> {
        self.inner.abort(reason).await.map_err(classify)
    }
}

impl PgTx<'_> {
    /// COPY runs only on a [`crate::native::PgSession`] transaction: a COPY abandoned half way (a dropped
    /// sink, an unread stream) leaves its connection mid-protocol, and a pooled connection would be handed
    /// to the next caller in that state (measured: `unexpected connection status`, `expected ParseComplete`).
    /// A dedicated connection just fails visibly and is discarded with its session.
    fn require_dedicated(&self) -> Result<(), RelError> {
        if self.dedicated {
            Ok(())
        } else {
            Err(RelError::Unsupported(
                "COPY needs a PgSession transaction (PgSession::begin), not a pooled one".into(),
            ))
        }
    }

    /// Start a COPY OUT and stream its data. Only on a [`crate::native::PgSession`] transaction.
    pub async fn copy_out(&mut self, copy: &CopyOut) -> Result<CopyOutStream<'_>, RelError> {
        self.require_dedicated()?;
        let conn: &mut sqlx::PgConnection = &mut self.tx;
        let inner = conn
            .copy_out_raw(&copy.statement())
            .await
            .map_err(classify)?;
        Ok(CopyOutStream { inner })
    }

    /// Start a COPY IN and return the sink to write its data to. Only on a [`crate::native::PgSession`]
    /// transaction.
    pub async fn copy_in(&mut self, copy: &CopyIn) -> Result<CopyInSink<'_>, RelError> {
        self.require_dedicated()?;
        let conn: &mut sqlx::PgConnection = &mut self.tx;
        let inner = conn
            .copy_in_raw(&copy.statement())
            .await
            .map_err(classify)?;
        Ok(CopyInSink { inner })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statements_are_fixed_shapes_over_quoted_names() {
        let t = Ident::qualified("public", "t\"x").unwrap();
        let cols = [Ident::new("a").unwrap(), Ident::new("b c").unwrap()];
        assert_eq!(
            CopyOut::table_columns(&t, &cols)
                .format(CopyFormat::Csv)
                .statement(),
            "COPY \"public\".\"t\"\"x\" (\"a\", \"b c\") TO STDOUT WITH (FORMAT csv)"
        );
        assert_eq!(
            CopyIn::table(&t).format(CopyFormat::Binary).statement(),
            "COPY \"public\".\"t\"\"x\" FROM STDIN WITH (FORMAT binary)"
        );
        let q = CopyOut::query(
            "SELECT {1} FROM {0}",
            &[crate::native::one(&t), crate::native::list(&cols)],
        )
        .unwrap();
        assert_eq!(
            q.statement(),
            "COPY (SELECT \"a\", \"b c\" FROM \"public\".\"t\"\"x\") TO STDOUT WITH (FORMAT text)"
        );
    }

    #[test]
    fn a_query_must_be_a_read() {
        for bad in [
            "DELETE FROM t",
            "COPY t TO STDOUT",
            "  insert into t values (1)",
            "",
        ] {
            assert!(CopyOut::query(bad, &[]).is_err(), "{bad:?}");
        }
        for ok in [
            "select 1",
            " WITH x AS (SELECT 1) SELECT * FROM x",
            "VALUES (1)",
        ] {
            assert!(CopyOut::query(ok, &[]).is_ok(), "{ok:?}");
        }
    }
}
