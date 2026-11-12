// SPDX-License-Identifier: MIT OR Apache-2.0
//! PostgreSQL-only surface that the backend-neutral traits deliberately lack
//! (`docs/DESIGN-PG-NATIVE-SESSION.md`): read-only and deferrable transactions ([`TxOptions`]) and
//! statements that carry identifiers ([`Ident`], [`IdentArg`], [`PgTx::query_ident`]).
//!
//! ## Rules, and why
//!
//! - **None of this is in `persistant-relational`.** SQLite and MySQL have no `READ ONLY` snapshot
//!   with these semantics and no quoted-identifier rules like PostgreSQL's; the portable traits stay
//!   portable.
//! - **No SQL is built from a caller's text.** An identifier enters a statement only as an [`Ident`]
//!   (validated, double-quoted, embedded quotes doubled), and only into a hole of a `&'static str`
//!   template: a runtime `String` cannot be the template, so a caller's text can never be the
//!   statement. Values still travel as `$n` parameters.
//! - **No free `SET` text, no raw connection out, no silent reconnect** (phase 2 of the design): each
//!   would let a caller's string reach the server as SQL, or lose session state unseen.

use crate::{PgBackend, PgTx, bind, classify, decode};
use persistant_relational::{Isolation, RelError, Row, Value};

/// PostgreSQL's `NAMEDATALEN - 1`: the longest identifier, in bytes.
const MAX_IDENT_BYTES: usize = 63;

/// One validated PostgreSQL identifier (or a `schema.name` pair), held in its quoted form.
///
/// ```
/// use persistant_sqlx::native::Ident;
/// assert_eq!(Ident::new("users").unwrap().as_sql(), "\"users\"");
/// assert_eq!(Ident::new("a\"b; DROP").unwrap().as_sql(), "\"a\"\"b; DROP\"");
/// assert_eq!(Ident::qualified("public", "t").unwrap().as_sql(), "\"public\".\"t\"");
/// assert!(Ident::new("").is_err());
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ident(String);

fn quote(part: &str) -> Result<String, RelError> {
    if part.is_empty() {
        return Err(RelError::Backend("identifier is empty".into()));
    }
    if part.contains('\0') {
        return Err(RelError::Backend("identifier contains a NUL".into()));
    }
    if part.len() > MAX_IDENT_BYTES {
        return Err(RelError::Backend(format!(
            "identifier is {} bytes; PostgreSQL's limit is {MAX_IDENT_BYTES}",
            part.len()
        )));
    }
    Ok(format!("\"{}\"", part.replace('"', "\"\"")))
}

impl Ident {
    /// A single identifier. Rejected: empty, containing NUL, longer than 63 bytes. Every other text is
    /// accepted and quoted, so a hostile name is data, not SQL.
    pub fn new(name: &str) -> Result<Ident, RelError> {
        quote(name).map(Ident)
    }

    /// `"schema"."name"`.
    pub fn qualified(schema: &str, name: &str) -> Result<Ident, RelError> {
        Ok(Ident(format!("{}.{}", quote(schema)?, quote(name)?)))
    }

    /// The quoted form, ready to sit in a statement.
    pub fn as_sql(&self) -> &str {
        &self.0
    }
}

/// What fills one `{n}` hole: one identifier, or a non-empty list joined with `, `.
#[derive(Debug, Clone, Copy)]
pub enum IdentArg<'a> {
    /// One [`Ident`].
    One(&'a Ident),
    /// Several, comma-joined, each quoted: a column list or an `ORDER BY` list.
    List(&'a [Ident]),
}

/// A one-identifier hole argument.
pub fn one(ident: &Ident) -> IdentArg<'_> {
    IdentArg::One(ident)
}

/// A list hole argument (column list, `ORDER BY` list).
pub fn list(idents: &[Ident]) -> IdentArg<'_> {
    IdentArg::List(idents)
}

/// Fill `{0}`, `{1}`, ... of a static template. `{{` and `}}` are literal braces. Every hole must
/// name an argument, every argument must be used, and a list must not be empty.
pub(crate) fn render(template: &'static str, args: &[IdentArg<'_>]) -> Result<String, RelError> {
    let bad = |m: String| RelError::Backend(format!("identifier template: {m}"));
    let mut out = String::with_capacity(template.len() + 16);
    let mut used = vec![false; args.len()];
    let mut it = template.char_indices().peekable();
    while let Some((i, c)) = it.next() {
        match c {
            '{' if matches!(it.peek(), Some((_, '{'))) => {
                it.next();
                out.push('{');
            }
            '}' if matches!(it.peek(), Some((_, '}'))) => {
                it.next();
                out.push('}');
            }
            '{' => {
                let rest = &template[i + 1..];
                let end = rest
                    .find('}')
                    .ok_or_else(|| bad("unclosed `{`".to_string()))?;
                let n: usize = rest[..end]
                    .parse()
                    .map_err(|_| bad(format!("`{{{}}}` is not a hole number", &rest[..end])))?;
                let arg = args
                    .get(n)
                    .ok_or_else(|| bad(format!("hole {{{n}}} has no argument")))?;
                used[n] = true;
                match arg {
                    IdentArg::One(id) => out.push_str(id.as_sql()),
                    IdentArg::List(ids) => {
                        if ids.is_empty() {
                            return Err(bad(format!("list for hole {{{n}}} is empty")));
                        }
                        let joined: Vec<&str> = ids.iter().map(Ident::as_sql).collect();
                        out.push_str(&joined.join(", "));
                    }
                }
                // skip past the closing brace
                for _ in 0..=end {
                    it.next();
                }
            }
            '}' => return Err(bad("stray `}`".to_string())),
            c => out.push(c),
        }
    }
    if let Some(n) = used.iter().position(|u| !u) {
        return Err(bad(format!("argument {n} is not used by the template")));
    }
    Ok(out)
}

/// Whether a transaction may write.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Access {
    /// The default.
    #[default]
    ReadWrite,
    /// `READ ONLY`: the server refuses any write.
    ReadOnly,
}

/// How a PostgreSQL transaction begins. Rendered by a closed `match` into
/// `BEGIN ISOLATION LEVEL .. [READ ONLY] [DEFERRABLE]`; no caller text reaches the server.
///
/// A `REPEATABLE READ` snapshot is taken by the **first statement**, not by `BEGIN`; by default
/// [`PgBackend::begin_with`] therefore runs one trivial statement so the snapshot is fixed when it
/// returns. [`TxOptions::snapshot_now`]`(false)` leaves it to the first statement of the caller.
///
/// There is no default statement timeout: a snapshot held for hours is a legitimate use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct TxOptions {
    /// The isolation level.
    pub isolation: Isolation,
    /// Read-only or read-write.
    pub access: Access,
    /// `DEFERRABLE`: valid only with `Serializable` and `ReadOnly`.
    pub deferrable: bool,
    /// Fix the snapshot at `begin` (see above). Default `true`.
    pub snapshot_now: bool,
}

impl TxOptions {
    /// Read-write, not deferrable, snapshot fixed at `begin`.
    pub fn new(isolation: Isolation) -> Self {
        TxOptions {
            isolation,
            access: Access::ReadWrite,
            deferrable: false,
            snapshot_now: true,
        }
    }

    /// `READ ONLY`.
    pub fn read_only(mut self) -> Self {
        self.access = Access::ReadOnly;
        self
    }

    /// `DEFERRABLE` (needs `Serializable` and `READ ONLY`, else `begin_with` returns `Unsupported`).
    pub fn deferrable(mut self) -> Self {
        self.deferrable = true;
        self
    }

    /// Whether `begin_with` fixes the snapshot before returning.
    pub fn snapshot_now(mut self, now: bool) -> Self {
        self.snapshot_now = now;
        self
    }

    pub(crate) fn statement(&self) -> Result<String, RelError> {
        let level = match self.isolation {
            Isolation::ReadCommitted => "READ COMMITTED",
            Isolation::RepeatableRead => "REPEATABLE READ",
            Isolation::Serializable => "SERIALIZABLE",
            other => return Err(RelError::Unsupported(format!("isolation level {other:?}"))),
        };
        if self.deferrable
            && !(self.isolation == Isolation::Serializable && self.access == Access::ReadOnly)
        {
            return Err(RelError::Unsupported(
                "DEFERRABLE needs SERIALIZABLE and READ ONLY".into(),
            ));
        }
        let mut s = format!("BEGIN ISOLATION LEVEL {level}");
        if self.access == Access::ReadOnly {
            s.push_str(" READ ONLY");
        }
        if self.deferrable {
            s.push_str(" DEFERRABLE");
        }
        Ok(s)
    }
}

impl PgBackend {
    /// Begin a transaction with [`TxOptions`]. `commit` and `rollback` on the result work at any
    /// later point, including after a failed statement inside it.
    pub async fn begin_with(&self, options: TxOptions) -> Result<PgTx<'_>, RelError> {
        let statement = options.statement()?;
        let mut tx = PgTx {
            tx: self
                .pool
                .begin_with(sqlx::AssertSqlSafe(statement))
                .await
                .map_err(classify)?,
        };
        if options.snapshot_now {
            // `SELECT 1` plans as a query, which takes the transaction snapshot.
            tx.execute_static("SELECT 1").await?;
        }
        Ok(tx)
    }
}

impl PgTx<'_> {
    async fn execute_static(&mut self, sql: &'static str) -> Result<(), RelError> {
        sqlx::query(sql)
            .execute(&mut *self.tx)
            .await
            .map(|_| ())
            .map_err(classify)
    }

    /// Run a statement that carries identifiers and return its rows. `template` is a `&'static str`
    /// (a caller's runtime text cannot be the statement) with `{0}`, `{1}`, ... holes filled by
    /// [`one`] / [`list`]; values go through `params` as `$n`.
    ///
    /// ```compile_fail
    /// # use persistant_sqlx::native::*;
    /// # async fn f(tx: &mut persistant_sqlx::PgTx<'_>, user_text: String) {
    /// // a runtime String is not a 'static template
    /// tx.query_ident(&user_text, &[], &[]).await.unwrap();
    /// # }
    /// ```
    pub async fn query_ident(
        &mut self,
        template: &'static str,
        idents: &[IdentArg<'_>],
        params: &[Value],
    ) -> Result<Vec<Row>, RelError> {
        let sql = render(template, idents)?;
        let q = bind(sqlx::query(sqlx::AssertSqlSafe(sql)), params)?;
        let rows = q.fetch_all(&mut *self.tx).await.map_err(classify)?;
        rows.iter().map(decode).collect()
    }

    /// [`PgTx::query_ident`] for a statement that returns no rows; yields the rows affected.
    pub async fn execute_ident(
        &mut self,
        template: &'static str,
        idents: &[IdentArg<'_>],
        params: &[Value],
    ) -> Result<u64, RelError> {
        let sql = render(template, idents)?;
        let q = bind(sqlx::query(sqlx::AssertSqlSafe(sql)), params)?;
        q.execute(&mut *self.tx)
            .await
            .map(|r| r.rows_affected())
            .map_err(classify)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting_and_limits() {
        assert_eq!(Ident::new("a b").unwrap().as_sql(), "\"a b\"");
        assert_eq!(Ident::new("x\"; --").unwrap().as_sql(), "\"x\"\"; --\"");
        assert!(Ident::new("a\0b").is_err());
        assert!(Ident::new(&"x".repeat(63)).is_ok());
        assert!(Ident::new(&"x".repeat(64)).is_err());
        // 63 bytes counted in bytes, not chars: 32 two-byte chars is 64 bytes
        assert!(Ident::new(&"é".repeat(32)).is_err());
        assert!(Ident::qualified("", "t").is_err());
    }

    #[test]
    fn templates() {
        let a = Ident::new("t").unwrap();
        let cols = [Ident::new("a").unwrap(), Ident::new("b\"c").unwrap()];
        assert_eq!(
            render("SELECT {1} FROM {0} ORDER BY {1}", &[one(&a), list(&cols)]).unwrap(),
            "SELECT \"a\", \"b\"\"c\" FROM \"t\" ORDER BY \"a\", \"b\"\"c\""
        );
        assert_eq!(render("SELECT '{{}}'", &[]).unwrap(), "SELECT '{}'");
        assert!(render("SELECT {0}", &[]).is_err(), "hole without argument");
        assert!(render("SELECT 1", &[one(&a)]).is_err(), "unused argument");
        assert!(render("SELECT {0", &[one(&a)]).is_err(), "unclosed");
        assert!(render("SELECT } {0}", &[one(&a)]).is_err(), "stray");
        assert!(render("SELECT {x}", &[one(&a)]).is_err(), "not a number");
        assert!(render("SELECT {0}", &[list(&[])]).is_err(), "empty list");
    }

    #[test]
    fn begin_statements() {
        let s = |o: TxOptions| o.statement();
        assert_eq!(
            s(TxOptions::new(Isolation::RepeatableRead).read_only()).unwrap(),
            "BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY"
        );
        assert_eq!(
            s(TxOptions::new(Isolation::ReadCommitted)).unwrap(),
            "BEGIN ISOLATION LEVEL READ COMMITTED"
        );
        assert_eq!(
            s(TxOptions::new(Isolation::Serializable)
                .read_only()
                .deferrable())
            .unwrap(),
            "BEGIN ISOLATION LEVEL SERIALIZABLE READ ONLY DEFERRABLE"
        );
        assert!(s(TxOptions::new(Isolation::RepeatableRead).deferrable()).is_err());
        assert!(s(TxOptions::new(Isolation::Serializable).deferrable()).is_err());
    }
}
