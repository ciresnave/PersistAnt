# Row 30, open half: the key-value layer against auth-framework's own PostgreSQL SQL: design note

Written 2026-11-24 BEFORE any run, as the cluster PM asked. The run, its result and anything this note
got wrong are recorded in `bench/pg-vs-auth-sql.txt`, never by editing this file afterwards.

## The question
`bench/pg-vs-sqlx.txt` answered the first half: PersistAnt's relational layer costs nothing visible next
to direct sqlx on the same statements. auth-framework's open question is the other half: its own
`PostgresStorage` (one table per record kind) against the same operations through
`PersistantStorage<KvOnRelational<PgBackend>>` (one key-value table plus index entries). That is a
different set of statements, and on the hot path a different NUMBER of statements.

## Source of the SQL (read-only, by SHA)
auth-framework @ `0cec4464`: `src/storage/postgres.rs` (the direct path), `src/storage/persistant.rs`
(the Persistant-backed path), schema in `PostgresStorage::migrate` and `src/migrations/mod.rs`
(`SESSIONS_SCHEMA_SQL`). pm-platform counted 31 query sites; they do not say which run per request.
I read the call paths. I cannot see auth-framework's callers from here, so "hot" is my reading of the
storage trait, stated as such, not a measured request mix.

## What is compared (four operations, each a pair)
| op | direct (auth-framework's own SQL, verbatim) | through PersistAnt (as `persistant.rs` calls it) | why picked |
|---|---|---|---|
| token check | `SELECT * FROM auth_tokens WHERE access_token = $1` | `Indexed::lookup_one(index key)`: one get of the index entry, one get of the record | every authenticated request validates a bearer token; the only op with a different statement count |
| session read | `SELECT data FROM sessions WHERE session_id = $1` | `kv.get("ses:<id>")` | every session-bound request |
| login: store token | `INSERT ... ON CONFLICT (token_id) DO UPDATE` (the 16-column statement) | `upsert`: `kv.get(primary)`, then `Indexed::insert` (one atomic batch: record + 2 index entries) | once per login and per refresh |
| kv get | `SELECT value FROM kv_store WHERE key=$1 AND (expires_at IS NULL OR expires_at > NOW())` | `kv.get("kv:<key>")` | rate-limit and nonce style reads |
Tables are created with auth-framework's own DDL (including the columns added by ALTER), indexes
included. Payloads: a token JSON of about 600 bytes and a session JSON of about 300, built with the
fields those types carry; the direct path binds the typed columns (arrays, JSONB) as it does.

## What is NOT compared (stated, not hidden)
- Not auth-framework's code: it cannot be depended on from here (published versions only), so the SQL and
  call shapes are copied and the benchmark is a model of its storage, not a run of it.
- No request mix, no concurrency (one connection, one operation at a time), no network beyond the
  loopback hop to the container, no table size beyond the 2000 seeded rows, no TTL expiry sweep.
- `list_user_tokens`, `count_active_sessions`, `swap_kv` and the other 25 sites are not measured.
- Direct side does the SQL only; it does not JSON-decode the row into `AuthToken` (the Persistant side
  does decode in auth-framework, but the bytes here are not decoded either: both sides stop at the bytes
  or columns), so the gap is storage, not serde.

## Method (built for a loaded box)
One PostgreSQL 16 container, one database, the same seeded data, same pool settings.
- Per operation, **interleaved pairs in one session**: A then B, then B then A, alternating which
  goes first, so order effects cancel. 5 pairs of warm-up are discarded.
- **At least 20 measured pairs per operation (target 40)**; one pair = 25 calls of each side timed as a
  batch (so a call of ~1 ms is not lost in clock resolution), mean ns per call per side.
- Statistic: the per-pair ratio persistant/direct. Report **n, median, and the spread (min, p25, p75,
  max)** of the ratio. No pooled mean of absolute times.
- **Load recorded at each pair** (outside the timed batch): count of `cargo`/`rustc` processes, count of
  `claude` processes, total CPU percent, and the Postgres container's CPU percent from `docker stats`.
- The claim rests on the paired ratios under that load. If the interquartile range of a ratio includes 1.0
  and spans more than 0.3, the row says "not separable on this box", and the PM picks a window. A
  statement count difference (2 vs 1) is reported as a count regardless of timing.
- Mutation check of the harness: a deliberately slowed side (a `pg_sleep(0.002)` on the direct path) must
  show a ratio well below 1; a control that runs the SAME statement as both sides must centre on 1.0.

## Deliverable
`crates/persistant-sqlx/examples/pg_vs_auth_sql.rs` and `bench/pg-vs-auth-sql.txt`; row 30 updated with
the figures, the load, the n and the spread, and what remains open.
