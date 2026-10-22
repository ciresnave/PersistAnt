# A sqlx-checked query macro that runs through UniSQL and PersistAnt (board 230, option 3)

Status: **design, 2026-10-22, nothing built.** Written against PersistAnt `a33ceb5` (0.9.10), UniSQL 0.24.17 and
Humboldt `f54c05c`. Origin: CireSnave on board 230, 2026-10-22, verbatim: *"On decision 230: I like your option (3)
that keeps the SQLx PostgreSQL checks in place but transitions everything to use the resulting SQL through
UniSQL/PersistAnt."* The PM's brief for the shape: the macro expands to (a) a hidden `sqlx::query!` / `query_as!` on
the same literal, so sqlx's own PostgreSQL check stays exactly as Humboldt has it, and (b) a `unisql_embed::Query`
constant executed through PersistAnt. Humboldt migrates module by module after the macro exists.

## 1. Why option 3, in numbers

Measured at the refs above by building one real `persistant::sql!(source = postgres, targets = [postgres], ..)` per
statement (`HUMBOLDT-MEASURE.md`, scratchpad of the PersistAnt lane; scripts beside it):

| | |
|---|---|
| `query!` / `query_as!` / `query_scalar!` literals in `backend/` | **883** (240 in `src/`, 643 in `tests/`; the "~399" in the brief matches neither) |
| accepted and translated by UniSQL | **881** |
| refused | **2**: one `LEFT JOIN LATERAL (subquery)` (`src/routes/dismissal_events.rs:1233`), one `::regclass` catalog query (a test) |
| given a table/column/constraint check by our checker | `INSERT` and `UPDATE .. SET` only: 25 of 240 `src/` statements fully, 65 in part |
| given **no** table or column check | 174 of 240 `src/` statements (every `SELECT` and `DELETE`) |
| `query_as!` structs that can `#[derive(FromRow)]` today | **0 of 24** (`Uuid`, `chrono`, `Vec<String>` fields are not `FromValue`) |

Positive control: a wrong column in an `INSERT` or `UPDATE` is caught by both sqlx and `sql!`; a wrong column or table in
a `SELECT` or `DELETE` is caught by sqlx and **not** by `sql!`. Dropping sqlx's check would therefore lose compile-time
coverage for about three statements in four. Keeping it loses nothing, which is the whole of option 3.

## 2. Shape

```rust
// in Humboldt, replacing sqlx::query_as!(ZoneDto, "SELECT ..", school_id).fetch_all(&pool)
let zones: Vec<ZoneDto> = persistant::sqlx_query_as!(ZoneDto, "SELECT id, name, .. WHERE school_id = $1", school_id)
    .fetch_all(&db).await?;
```

Three macros, same argument shape as sqlx's (`query!`, `query_as!`, `query_scalar!`), so a module migrates by changing
the path and the executor: `persistant::sqlx_query!`, `sqlx_query_as!`, `sqlx_query_scalar!`. Each expands to one block:

1. **The check, never run.** `let _ = || { ::sqlx::query_as!(ZoneDto, "<same literal>", school_id) };`: a closure that is
   never called, so sqlx's macro runs at build time exactly as today (live `DATABASE_URL` or `SQLX_OFFLINE`/`.sqlx`),
   type-checks the arguments against the server's parameter types and the struct against its columns, and costs nothing at run
   time. A wrong column, a wrong type, an argument count mismatch: sqlx's error, unchanged. (The expansion must not
   hide sqlx's span; the macro passes the literal token through untouched.)
2. **The statement.** `const Q: persistant::Sql = persistant::sql!(source = postgres, targets = [postgres, ..], "<literal>")`,
   so UniSQL's checks and translation also run at build time. A refusal is a compile error that names the construct and
   the way out (`allow = [..]` or a UniSQL fix).
3. **The run.** A builder value carrying `&Q`, the bound arguments and the row decoder, with `fetch_one`,
   `fetch_optional`, `fetch_all`, `execute` (the four Humboldt uses: 124, 74, 86 and the rest in `src/`) taking an
   executor (section 6).

## 3. Typed output: one source of truth, no double maintenance

The risk the PM named. UniSQL cannot infer result types or nullability for joins, aggregates and expressions; sqlx can,
because it asks the server. Two ways to type rows would mean two things to keep in step. So **sqlx's description is the
only one**, and `sql!`'s own schema snapshot is not needed for typing.

sqlx's offline data (`.sqlx/query-<sha256 of the literal>.json`, which Humboldt already commits and keeps current:
566 files, `SQLX_OFFLINE=true` in CI) holds, per statement, the column names, PostgreSQL types, per-column nullability
and the parameter types:

```json
{ "query": "SELECT count(*) AS \"c!\" FROM students WHERE id = $1",
  "describe": { "columns": [{ "ordinal": 0, "name": "c!", "type_info": "Int8" }],
                "parameters": { "Left": ["Uuid"] }, "nullable": [null] } }
```

The macro reads the file for its own literal (same directory rule as sqlx: `SQLX_OFFLINE_DIR`, else the crate's or
workspace's `.sqlx`). It is **a compile error if the file is missing or stale** (the query text is the hash, so a changed
literal has no file): the message says `cargo sqlx prepare`. That is one added requirement over sqlx with a live
database, and none for Humboldt, which already works offline.

* **`sqlx_query_as!(T, ..)`**: `T` is Humboldt's existing DTO, declared once. sqlx's hidden `query_as!` checks it
  against the server (names, types, nullability, by name). Ours decodes by position through `T: FromRow`, so the macro
  also emits a `const` assertion that the described column names, in order, equal `T::FIELDS` (the machinery of
  `persistant_relational::row::check` already does this). A `FromRow` derive is only needed on `T`; no second schema.
* **`sqlx_query!`** (Humboldt reads `row.id`, `row.name` on sqlx's anonymous record): the macro **generates the record
  struct** from the described columns, with sqlx's own type mapping and nullability rules (`Int8`->`i64`, `Uuid`->`Uuid`,
  `Timestamptz`->`chrono::DateTime<Utc>`, `Bool`, `Text`/`Varchar`->`String`, `Bytea`->`Vec<u8>`, arrays->`Vec<T>`,
  `"col!"` / `"col?"` / `"col: Type"` overrides honoured, nullable -> `Option<T>`). Call sites do not change.
* **`sqlx_query_scalar!`**: the single described column, decoded as that type.

What the decoder needs that does not exist yet (follow-up B, section 8): `FromValue` for `uuid::Uuid`, `chrono`
(`DateTime<Utc>`, `NaiveDate`, `NaiveDateTime`, `NaiveTime`), `serde_json::Value`, `rust_decimal`/`bigdecimal` if used,
behind cargo features, reading the canonical text the backends already return (`docs/DECODER-AUDIT.md`). **PostgreSQL
arrays (`Vec<String>`, 2 fields in Humboldt) are loud in the driver today**; either the driver learns `text[]`/`uuid[]`
or those columns are read through `array_to_json` (a statement edit). Decide when the first such module migrates.

## 4. Parameters

Today `Value` has six variants and the PostgreSQL driver binds `Value::Text` as `text`, so `WHERE id = $1` with a UUID
bound as text fails ("operator does not exist: uuid = text"). Two options:

* **(recommended) Cast in the statement the macro emits, from the described parameter types.** `.sqlx` says `$1` is
  `Uuid`, so the macro rewrites that placeholder to `CAST($1 AS uuid)` (`timestamptz`, `date`, `numeric`, `jsonb`
  likewise) in the text handed to UniSQL. The sqlx check still sees the original literal. `Value` stays closed, and the
  same statement works on targets whose driver also binds text. Arguments convert with a `ToValue` trait (`Uuid` ->
  canonical text, `DateTime<Utc>` -> RFC 3339, `Option<T>` -> `Null`), the mirror of `FromValue`.
* Add typed `Value` variants (`Uuid`, `Timestamp`, ..). Breaking for every backend and conformance test for a
  PostgreSQL-only gain; not recommended.

The argument count and each argument's Rust type are checked by the hidden sqlx call (it is given the arguments); our
macro adds a `ToValue` bound per argument, so a type sqlx accepts but we cannot convert is a compile error naming it.

## 5. Dialect targets

`source = postgres` always (the literal is what sqlx checks). `targets` default for the three macros: `[postgres]`, the
one Humboldt runs; a crate that wants more writes `targets = [postgres, sqlite]` per call, or a one-line wrapper. A
statement UniSQL cannot translate for a named target is a compile error, so the list is a promise. Nothing here makes
a Postgres-only statement portable; it only stops pretending: `LATERAL`, `::regclass`, `FOR UPDATE`, array operators
stay Postgres, and are refused for other targets (measured: 2 of 883 refused for Postgres itself).

## 6. Executors and transactions

Humboldt holds a `PgPool` (151 uses in `src/`) and passes `&mut *tx` of a sqlx `Transaction` in 187 places. A module
cannot move one statement at a time if a sqlx transaction spans the old and new statements. So the builder's `fetch_*`
and `execute` accept any `Runner`, implemented for:

1. **PersistAnt's `RelationalBackend` and `RelationalTx`** (the default, through `RelationalSqlExt`): the destination.
   `PgBackend::from_pool(pool)` already shares Humboldt's pool, so a module can own its transactions through
   `begin(Isolation)` once all its statements are ported.
2. **sqlx executors** (`&PgPool`, `&mut PgConnection`, `&mut Transaction`), feature `sqlx-bridge`: runs the translated
   PostgreSQL text with sqlx's own binds (`query(text).bind(uuid)`, with the real types, since the macro knows each
   parameter's type) and decodes the rows through the same `FromRow`. This is the migration ramp: replace the macro
   path first, executor later, statement by statement, inside existing transactions. It is **not** C6 (a
   `connection()` accessor on persistant's transaction types): no raw connection is exposed, and C6 stays unbuilt.

Row decoding is identical on both, so a statement moves between them with no change to its call site beyond the executor
argument.

## 7. Offline data and CI

Unchanged for Humboldt: `.sqlx/` is committed and the build runs with `SQLX_OFFLINE=true`. (Its `ci.yml` has no
`cargo sqlx prepare --check` step, only a comment that the data is kept current by hand; a stale file already fails the
offline build, and ours fails the same way, but adding the `--check` step is cheap and is Humboldt's call.) The macro adds the dependency on the same files and no new CI service. A project that checks
against a live database instead sets `DATABASE_URL`; then it needs `cargo sqlx prepare` for the typed forms (section 3),
and the plain `execute` form (no row typing) needs nothing beyond sqlx's own check. Rebuild tracking: the expansion
emits `include_bytes!` of the `.sqlx` file it read, so a refreshed file rebuilds the crate, the same pattern as the
schema snapshot.

## 8. Work, in order (each a PR; none started)

| # | PR | Why first |
|---|---|---|
| A | `FromValue` / `ToValue` for `Uuid`, `chrono`, `serde_json::Value` (feature-gated) | every typed form needs them; this is also the PM's "Uuid/chrono/Vec in FromRow" follow-up |
| B | The `.sqlx` reader and record generator (a library, no macro yet), tested against Humboldt's 566 files | proves the type mapping against real data before any macro depends on it |
| C | `sqlx_query_scalar!`, `sqlx_query!`, `sqlx_query_as!` with the hidden sqlx call, the `Sql` const, the `Runner` trait, impl 1 (PersistAnt backends) | the macro itself; compile-fail tests for a stale file, a wrong column (sqlx's error must survive), an unconvertible argument |
| D | `sqlx-bridge` runner (impl 2) | needed before Humboldt can migrate any module that holds a sqlx transaction |
| E | Checker: name checks for `SELECT` / `DELETE` (and the join/alias scope rules the design promised and 0.9.x did not build) | separate row (PM); value is for non-sqlx consumers, since here sqlx already checks |
| F | Option A items: migration loader for Humboldt's files, fresh-install verifier, expiry sweeps | the PM's list; independent of the macro |

UniSQL asks: translate `LEFT JOIN LATERAL (subquery)` (the one `src/` refusal) and the `::regclass` cast, or document the
`allow` list Humboldt should use for those two.

## 9. Not decided here

* **Name.** `sqlx_query!` family under `persistant::` is a placeholder; Humboldt might prefer shadowing `query!` with a
  `use persistant::sqlx_query as query` so call sites do not change at all.
* **Whether the hidden sqlx call is permanent.** CireSnave's ruling keeps it; the measurement says it carries most of
  the checking, so it stays until E and a join-aware typing layer exist, which this design does not promise.
* **Run-time cost.** sqlx caches prepared statements per connection; the persistant path prepares the same text, so no
  difference is expected, but it is unmeasured.
