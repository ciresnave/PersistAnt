# One dedicated Postgres connection with session state: design note

Status: **design, 2026-11-12, nothing built.** Written against PersistAnt `26370a9` (0.17.1). Origin: PM [TASK] relaying
Humboldt's transfer tool (`db-transfer`), which needs one non-pooled connection that keeps session state, a
long-lived snapshot transaction, dynamic text SQL and COPY streaming. Not a build request; the PM answers first.

## 1. What is asked, and what already exists

| Need | Today (0.17.1) |
|---|---|
| macros and advisory locks inside a transaction Humboldt already holds | **covered**: `OnConn(&mut *tx)`, `advisory::{lock,try_lock,lock64,try_lock64}(&mut PgConnection, ..)` |
| dynamic-text execute/fetch | **covered at the trait**: `RelationalTx::{execute,query}(&str, &[Value])`; no safe way to put an *identifier* in |
| `begin(RepeatableRead)` | covered; **read-only is not expressible** (`PgBackend::begin` has three fixed `BEGIN` strings) |
| one connection, session state (`DateStyle`, `TimeZone`, `extra_float_digits`, `IntervalStyle`) | **missing**: `PgBackend` is a pool, a `SET` lands on whichever connection ran it |
| COPY out / in | **missing** |

## 2. Does it belong in persistant-sqlx? Yes, as a Postgres-only module, not in the traits

`RelationalBackend` / `RelationalTx` are the portable surface (SQLite, MySQL, Postgres run the same conformance
leaves). A single connection with session state, `READ ONLY` snapshots and COPY have no portable meaning (MySQL has no
COPY; SQLite has no session), so none of it goes into `persistant-relational`. It goes in `persistant_sqlx::native`
(Postgres only, next to `PgBackend`), reusing `Isolation`, `ConnectionSecret`, `ConnectOptions`, `TlsPolicy`, the error
classifier and the secret-redaction guarantees (`connect_error_leak`). The new types implement `RelationalTx` where
they can, so `InTx` and the `persistant::sqlx_query!` ramp work on them unchanged.

## 3. Shape (what I would build)

```rust
let mut s = PgSession::connect(&secret, &opts, SessionSettings::new()
        .date_style_iso().time_zone_utc().extra_float_digits(3).interval_style_iso8601()).await?;
let mut tx = s.begin(TxOptions::new(Isolation::RepeatableRead).read_only()).await?;   // &mut s: one txn at a time
let rows = tx.query_ident("SELECT count(*) FROM {0}", &[&Ident::qualified("public", t)?], &[]).await?;
let mut out = tx.copy_out(CopyOut::table(&qualified).format(CopyFormat::Binary)).await?;  // Stream<Item=Result<Bytes>>
let mut sink = tx.copy_in(CopyIn::table(&qualified).format(CopyFormat::Binary)).await?;
sink.send(bytes).await?; let n: u64 = sink.finish().await?;
```

**Connection.** `PgSession` owns one `sqlx::PgConnection`, never a pool, and **never reconnects silently** (a new
connection has lost the session state and the snapshot; a dead connection is an error the caller sees). `begin(&mut self)`
borrows it, so a second concurrent transaction does not compile. No `&mut PgConnection` is handed out.

**Session settings: a closed set, no free `SET` text.** `SessionSettings` is a builder over an allow-list, each a typed
value rendered by us: `DateStyle` (ISO only), `TimeZone` (`Utc` or a name checked against `[A-Za-z0-9_/+-]{1,64}`),
`extra_float_digits` (-15..=3), `IntervalStyle` (enum), `statement_timeout`, `lock_timeout`,
`idle_in_transaction_session_timeout` (a `Duration`), `application_name` (exists). They are sent as **startup
parameters** (sqlx `PgConnectOptions::options`; the exact method to be checked against the sqlx version at build time),
so no `SET` statement text exists at all. **Built (phase 2) with one correction:** sqlx fixes `DateStyle` and `TimeZone` in its
own startup packet and that wins over `-c`, so those two are applied right after connect with
`SELECT set_config($1, $2, false)` (name from the allow-list, value checked, both bound parameters): still no SQL text
built from a value. The others (`IntervalStyle`, the timeouts, `extra_float_digits`) are startup parameters. Refused: any other GUC, `SET ROLE`, `SET SESSION AUTHORIZATION`,
`search_path` (a caller who needs it qualifies identifiers instead).

**Isolation and read-only.** `TxOptions { isolation: Isolation, access: ReadWrite|ReadOnly, deferrable: bool }`
(`#[non_exhaustive]`, builder). Rendered by a closed `match` into `BEGIN ISOLATION LEVEL .. [READ ONLY] [DEFERRABLE]`;
`deferrable` without `Serializable` + `ReadOnly` is `RelError::Unsupported` before anything is sent. Caveat for the docs
and a test: a `REPEATABLE READ` snapshot is taken at the **first statement**, not at `BEGIN`, so `begin` runs one trivial
statement by default (`TxOptions::snapshot_now()`, on by default) and "the snapshot lasts the whole run" holds from
`begin` returning. Sharing one snapshot across several connections (`pg_export_snapshot`) is a later row, not part of this.

**Identifiers: `Ident`, never string-built SQL.** `Ident::new(&str)` rejects empty, NUL and > 63 bytes (Postgres
`NAMEDATALEN - 1`), and renders as `"` + text with `"` doubled + `"`. `Ident::qualified(schema, name)` for the two-part
form. Dynamic statements are `query_ident(template: &'static str, idents: &[&Ident], params: &[Value])`: the template
must be a **`&'static str`** (a caller's runtime text cannot be the template), `{0}`, `{1}` are the only holes and take
only `Ident`s or a non-empty `&[Ident]` list (comma-joined, each quoted: variable column and `ORDER BY` lists), and every value still goes through `$n` parameters. The static-template rule is the guard; a compile-fail
doctest shows a `String` template refused. `SHOW` is `show(name)` with the name checked against `[a-z_.]+`. Catalog
reads are ordinary static templates with ident holes and parameters.

**COPY.** Only `STDIN` / `STDOUT`: server-side file and `PROGRAM` forms are never emitted. Out is
`copy_out(CopyOut) -> impl Stream<Item = Result<Bytes, RelError>> + '_` over sqlx's `copy_out_raw`, borrowing the
transaction mutably (nothing else runs until the stream is dropped; dropping early drains/cancels the copy, tested).
In is a `CopyIn` sink: `send(Bytes)`, `finish() -> u64` (rows), `abort(reason)`; **dropping without `finish` aborts**,
so a half-written COPY cannot commit by accident. Source/target is a table (+ optional column `Ident`s) or a static
template with ident holes; format is a closed `CopyFormat { Text, Csv, Binary }` with fixed options, no free `WITH (..)`
text. Errors go through the existing classifier; the connection secret is never echoed.

## 4. What I would refuse

- Any of this in `RelationalBackend`/`RelationalTx`/`persistant-relational`: it must stay portable.
- A free `SET <text>` or `execute_raw(String)` on the session; a pooled variant; auto-reconnect; exposing the raw
  `PgConnection` (that is the escape hatch below, which Humboldt can already do).
- Server-side `COPY .. TO/FROM 'file'|PROGRAM`, `DO` blocks, `LISTEN`, and replication-protocol use.

## 5. The honest alternative: Humboldt keeps a raw sqlx `PgConnection`

For one tool this is acceptable and costs us nothing: `PgConnectOptions` + `connect()`, `begin_with("BEGIN ISOLATION
LEVEL REPEATABLE READ READ ONLY")`, `copy_out_raw`, `OnConn(&mut *tx)` for the macros and the advisory functions. What it
gives up, and what the native module would give back: (a) `ConnectionSecret` handling and the guarantee that connect
errors never carry the URL; (b) `Ident` quoting (they would hand-roll `format!("\"{}\"", ..)`, the string-built SQL the PM
wants avoided); (c) typed, allow-listed session settings; (d) classified errors; (e) COPY drop-aborts-the-copy. If
Humboldt needs to ship before this exists, the raw connection is the right interim; nothing here blocks it or is blocked
by it.

## 6. Recommendation and phasing

Build `native`, in this order, each its own PR with a red-to-green test against a real server (the existing
`PERSISTANT_TEST_DATABASE_URL` jobs): **(1)** `Ident` + `TxOptions` (read-only, snapshot-at-begin) on the existing
`PgBackend::begin`, which also fixes the missing read-only for everyone; **(2)** `PgSession` + `SessionSettings`
(startup parameters, no reconnect, `RelationalTx` impl, `query_ident`, `show`); **(3)** COPY, last and only once Humboldt
confirms the exact signature, because it is the largest surface and the one raw sqlx already serves well. All three are
additive (minor or patch); none breaks a published API. Open for the PM/Humboldt: whether `pg_export_snapshot` (parallel
workers on one snapshot) is wanted, and whether `statement_timeout` belongs in the allow-list for a run that holds a
snapshot for hours.

## 7. Amendments after Humboldt's read (2026-11-12)

- Phase 1 alone is not enough for the transfer tool: its source side needs one connection that also holds `SET`s, so it
  uses the native surface after phase 2. Phases 1 and 2 go in order and quickly; phase 3 (COPY) is announced before it starts.
- `date_style_iso()` pins the exact string `ISO, MDY`. `CopyFormat::Text` keeps the current column list and NULL semantics.
- Dropping a COPY stream early is fine; no default timeout of any kind is added (a long snapshot is the point).
- `commit()` and `rollback()` work after a long gap and after an error inside the transaction (tested in phase 1).
