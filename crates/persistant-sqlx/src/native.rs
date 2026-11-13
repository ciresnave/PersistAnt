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

/// `IntervalStyle`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntervalStyle {
    /// `postgres` (the server default).
    Postgres,
    /// `postgres_verbose`.
    PostgresVerbose,
    /// `sql_standard`.
    SqlStandard,
    /// `iso_8601`.
    Iso8601,
}

impl IntervalStyle {
    fn as_str(self) -> &'static str {
        match self {
            IntervalStyle::Postgres => "postgres",
            IntervalStyle::PostgresVerbose => "postgres_verbose",
            IntervalStyle::SqlStandard => "sql_standard",
            IntervalStyle::Iso8601 => "iso_8601",
        }
    }
}

/// The session settings a [`PgSession`] may carry: a **closed allow-list**, each a typed value we
/// render. Most are sent as connection startup parameters; `DateStyle` and `TimeZone` (which sqlx
/// fixes in its own startup packet) are set right after connect with `set_config($1, $2, false)`,
/// the value a bound parameter. So no `SET` statement text is ever built from a value. There is deliberately no way to set any other parameter, and
/// no default timeout of any kind: a snapshot held for hours is a legitimate use.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct SessionSettings {
    date_style: bool,
    time_zone: Option<String>,
    extra_float_digits: Option<i8>,
    interval_style: Option<IntervalStyle>,
    statement_timeout: Option<std::time::Duration>,
    lock_timeout: Option<std::time::Duration>,
    idle_in_transaction_session_timeout: Option<std::time::Duration>,
}

impl SessionSettings {
    /// No setting beyond the server's and sqlx's own.
    pub fn new() -> Self {
        Self::default()
    }

    /// `DateStyle` = exactly `ISO, MDY`.
    pub fn date_style_iso(mut self) -> Self {
        self.date_style = true;
        self
    }

    /// `TimeZone` = `UTC`.
    pub fn time_zone_utc(self) -> Self {
        self.time_zone("UTC").expect("UTC is a valid time zone")
    }

    /// `TimeZone` = a zone name (letters, digits, `_ / + -`, 1 to 64 bytes: `Europe/Paris`, `UTC`,
    /// `Etc/GMT+5`). Anything else is refused here, before a connection exists.
    pub fn time_zone(mut self, name: &str) -> Result<Self, RelError> {
        let ok = !name.is_empty()
            && name.len() <= 64
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'/' | b'+' | b'-'));
        if !ok {
            return Err(RelError::Backend(format!(
                "time zone {name:?} is not a plain zone name"
            )));
        }
        self.time_zone = Some(name.to_string());
        Ok(self)
    }

    /// `extra_float_digits` (-15..=3).
    pub fn extra_float_digits(mut self, digits: i8) -> Result<Self, RelError> {
        if !(-15..=3).contains(&digits) {
            return Err(RelError::Backend(format!(
                "extra_float_digits {digits} is outside -15..=3"
            )));
        }
        self.extra_float_digits = Some(digits);
        Ok(self)
    }

    /// `IntervalStyle`.
    pub fn interval_style(mut self, style: IntervalStyle) -> Self {
        self.interval_style = Some(style);
        self
    }

    /// `statement_timeout`. Not set unless you ask.
    pub fn statement_timeout(mut self, d: std::time::Duration) -> Self {
        self.statement_timeout = Some(d);
        self
    }

    /// `lock_timeout`. Not set unless you ask.
    pub fn lock_timeout(mut self, d: std::time::Duration) -> Self {
        self.lock_timeout = Some(d);
        self
    }

    /// `idle_in_transaction_session_timeout`. Not set unless you ask.
    pub fn idle_in_transaction_session_timeout(mut self, d: std::time::Duration) -> Self {
        self.idle_in_transaction_session_timeout = Some(d);
        self
    }

    /// What must be set after connect: the allow-listed name and its checked value.
    pub(crate) fn after_connect(&self) -> Vec<(&'static str, String)> {
        let mut o = Vec::new();
        if self.date_style {
            o.push(("DateStyle", "ISO, MDY".to_string()));
        }
        if let Some(z) = &self.time_zone {
            o.push(("TimeZone", z.clone()));
        }
        o
    }

    /// The `-c name=value` startup options, values rendered by us (a space is escaped by sqlx).
    /// `DateStyle` and `TimeZone` are not here: sqlx sends both in its own startup packet, which wins
    /// over `-c`, so [`SessionSettings::after_connect`] sets them.
    pub(crate) fn startup_options(&self) -> Vec<(&'static str, String)> {
        let ms = |d: std::time::Duration| d.as_millis().to_string();
        let mut o = Vec::new();
        if let Some(s) = self.interval_style {
            o.push(("IntervalStyle", s.as_str().to_string()));
        }
        if let Some(d) = self.statement_timeout {
            o.push(("statement_timeout", ms(d)));
        }
        if let Some(d) = self.lock_timeout {
            o.push(("lock_timeout", ms(d)));
        }
        if let Some(d) = self.idle_in_transaction_session_timeout {
            o.push(("idle_in_transaction_session_timeout", ms(d)));
        }
        o
    }
}

/// One dedicated, non-pooled PostgreSQL connection that keeps its session state for its whole life.
///
/// It **never reconnects**: a new connection would have lost the session settings and any snapshot, so
/// a dead connection is an error the caller sees (later calls keep failing). [`PgSession::begin`]
/// borrows the session mutably, so two transactions at once do not compile. No `&mut PgConnection`
/// is handed out (that is the escape hatch: use sqlx's own connection).
pub struct PgSession {
    conn: sqlx::PgConnection,
}

impl std::fmt::Debug for PgSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PgSession").finish_non_exhaustive()
    }
}

impl PgSession {
    /// Connect once, applying `settings` as startup parameters. TLS policy and application name come
    /// from `options` as for [`PgBackend::connect_with`]; the pool fields of `options` do not apply.
    /// The connection string is resolved here and never part of an error.
    pub async fn connect(
        secret: &persistant_relational::ConnectionSecret,
        options: &crate::ConnectOptions,
        settings: SessionSettings,
    ) -> Result<Self, RelError> {
        use sqlx::ConnectOptions as _;
        let mut o = crate::options::pg_connect_options(secret, options)?;
        if let Some(n) = settings.extra_float_digits {
            o = o.extra_float_digits(n);
        }
        let startup = settings.startup_options();
        if !startup.is_empty() {
            o = o.options(startup);
        }
        let mut conn = o.connect().await.map_err(classify)?;
        for (name, value) in settings.after_connect() {
            // `false`: for the session, not just the current transaction.
            sqlx::query("SELECT set_config($1, $2, false)")
                .bind(name)
                .bind(value)
                .execute(&mut conn)
                .await
                .map_err(classify)?;
        }
        Ok(PgSession { conn })
    }

    /// Begin a transaction on this connection (read-only, snapshot at begin, ... per `options`). The
    /// result is the ordinary [`PgTx`]: `commit`, `rollback`, `query_ident`, the macros through
    /// `OnConn` are all as on a pooled transaction.
    pub async fn begin(&mut self, options: TxOptions) -> Result<PgTx<'_>, RelError> {
        use sqlx::Connection as _;
        let statement = options.statement()?;
        let mut tx = PgTx {
            tx: self
                .conn
                .begin_with(sqlx::AssertSqlSafe(statement))
                .await
                .map_err(classify)?,
        };
        if options.snapshot_now {
            tx.execute_static("SELECT 1").await?;
        }
        Ok(tx)
    }

    /// `SHOW <name>` for a setting name made of `[a-z_.]` only, outside any transaction.
    pub async fn show(&mut self, name: &str) -> Result<String, RelError> {
        if name.is_empty()
            || !name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || matches!(b, b'_' | b'.'))
        {
            return Err(RelError::Backend(format!(
                "setting name {name:?} is not [a-z_.]+"
            )));
        }
        // `name` is checked above: lowercase letters, `_` and `.` cannot end a statement.
        let sql = format!("SHOW {name}");
        sqlx::query_scalar::<_, String>(sqlx::AssertSqlSafe(sql))
            .fetch_one(&mut self.conn)
            .await
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
    fn session_settings_are_a_closed_typed_set() {
        let s = SessionSettings::new()
            .date_style_iso()
            .time_zone("Europe/Paris")
            .unwrap()
            .interval_style(IntervalStyle::Iso8601)
            .statement_timeout(std::time::Duration::from_secs(2));
        assert_eq!(
            s.startup_options(),
            vec![
                ("IntervalStyle", "iso_8601".to_string()),
                ("statement_timeout", "2000".to_string()),
            ]
        );
        assert_eq!(
            s.after_connect(),
            vec![
                ("DateStyle", "ISO, MDY".to_string()),
                ("TimeZone", "Europe/Paris".to_string()),
            ]
        );
        assert!(
            SessionSettings::new().startup_options().is_empty()
                && SessionSettings::new().after_connect().is_empty(),
            "no default setting, no timeout"
        );
        for bad in ["", "a b", "x;y", "UTC'--", &"z".repeat(65)] {
            assert!(SessionSettings::new().time_zone(bad).is_err(), "{bad:?}");
        }
        assert!(SessionSettings::new().extra_float_digits(4).is_err());
        assert!(SessionSettings::new().extra_float_digits(-16).is_err());
        assert!(SessionSettings::new().extra_float_digits(3).is_ok());
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
