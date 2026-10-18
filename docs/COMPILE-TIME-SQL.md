# Compile-time SQL: re-exporting UniSQL's macros with catalog-based checks

Status: **design, 2026-10-18, not built.** Written against PersistAnt 0.8.3 (`4febf59`) and UniSQL 0.24.11
(`a5cb56e`). Origin: PM [TASK], quoting CireSnave (`CIRESNAVE-EXPECTATIONS.md` 6.16): *"have PersistAnt
re-export it and the catalog-based checks. ... All of our projects should strive to provide that same
'checks happen at compile time' functionality where applicable."*

Nothing here is frozen until the UniSQL lane has agreed the interface in section 3 (they own the macro
crate, the `Schema` type and the checks; this repo owns producing the schema and the runtime glue).

## 0. The split of work

| | UniSQL | PersistAnt |
|---|---|---|
| Macro crate `unisql-macros` (translate per enabled target, syntax, placeholder count, refusal) | **owns** | re-exports |
| `Schema` type, the checks that read it (table/column existence, arity, ...) | **owns** (UniSQL resolves scopes and aliases; `schema.rs` says so) | supplies the data |
| Schema snapshot **file format** | **owns** (serialises its own `Schema`) | writes it |
| Producing a snapshot: from migration files, from a live catalog | `schema_from_ddl` exists | **owns** the catalog readers (UniSQL ROADMAP: "the live catalog reader is PersistAnt's, in its sqlx driver crate") and the `persistant-migrate snapshot` verb |
| Running a translated statement on a backend | - | **owns** (`Sql` value and `execute_sql`/`query_sql`, section 3.3) |

## 1. How a schema snapshot is produced (question a)

Three sources, one output: UniSQL's `Schema` serialised to a checked-in file (default
`persistant-schema.json` at the crate root, the same idea as sqlx's `.sqlx/` offline data).

1. **From the migration files, no database** (`unisql::schema_from_ddl(migrations concatenated in
   order, dialect)`). Works anywhere, including a CI job with no service container. Its limit is the
   DDL it can read (`CREATE TABLE`, `ALTER TABLE .. ADD|DROP|ALTER COLUMN .. TYPE`, `DROP TABLE`);
   anything else lands in `DdlSchema.skipped`, which the verb prints and which makes a column
   *unknown*, never absent. This is the default source for the snapshot a developer commits.
2. **From a live catalog** (new, in `persistant-sqlx`): `information_schema` for PostgreSQL and
   MySQL/MariaDB, `PRAGMA table_info` / `index_list` for SQLite, producing the same `Schema`
   **plus** what DDL parsing cannot know for certain: real declared types, nullability, defaults, and
   the unique keys the server actually has. It is the ground truth.
3. **Scratch apply** (`persistant-migrate snapshot --scratch --url ...`): apply the migrations to an
   empty scratch schema or database (the `verify_fresh` machinery of 0.8.x, now on all four engines),
   read source 2 from it, drop it. This is how the catalog is produced *from migrations* without
   touching the real target. Not offered through an in-memory SQLite for PostgreSQL-dialect
   migrations: a PostgreSQL migration is not SQLite SQL, so that would verify a different database.

`snapshot --check` re-derives and **fails when the committed file differs** (the same pattern as the
matrix files). Run with source 3 in the engine jobs that already have a service container, it catches
the one real risk of source 1: the DDL reader and the server disagreeing. A developer without Docker
still gets source 1.

Every verb that connects keeps the `--allow-remote` guard (0.8.3). The snapshot never contains data,
only structure, so committing it is safe.

**Needs from UniSQL:** a `Schema` that can be enumerated and serialised (today it has builders and
lookups, no iteration or serde), and `Column` fields beyond `class`: `declared_type`, `nullable`,
`has_default` (`Column` is `#[non_exhaustive]`, so this is additive). The file carries them from the
first version so that stage 3 of section 5 does not change the format. A `format` integer in the file
makes a too-new file a compile error rather than a silent downgrade.

## 2. What it checks without a server, and what it never claims (question b)

The rule for every check: **it errors only when it is certain.** A name the schema does not know about
(a view, a CTE column, a table in another schema, a skipped DDL statement) is *unknown* and passes.
A false compile error in a consumer's build is worse than a missed check.

| Check | Needs | Stage |
|---|---|---|
| Syntax valid in the source dialect; every enabled target can translate it (refusal by leaf id is a compile error) | nothing (UniSQL, no schema) | 1 |
| `$n` placeholders are `1..=k` with no gaps, and the argument count at the call site equals `k` | nothing | 1 |
| Table exists (base tables only) | schema | 2 |
| Column exists, resolved through aliases and joins by UniSQL's scope rules | schema | 2 |
| `INSERT` column list and `VALUES` arity agree; no duplicate column; `UPDATE .. SET` names real columns | schema | 2 |
| `NOT NULL` column without a default left out of an `INSERT` | nullability, default | 2 |
| `ON CONFLICT (cols)` names a stated unique key | keys (UniSQL already reads them) | 2 |
| Comparison operand classes (numeric vs text) for plain column refs | `ColumnClass` (exists) | 2 |
| A bound argument's Rust type fits the column's class | declared type | 3 |
| Selected columns fit a row struct (names, arity, class) | declared type, nullability | 3 |

**Deliberately not done**, and said in the macro's documentation: full PostgreSQL semantic typing
(function signatures, operator resolution, implicit casts, `search_path`), result types of
expressions, join cardinality, whether a `WHERE` is satisfiable, row-level security, anything about
data. UniSQL's own stance is "transpiler, not a validator" (`DESIGN.md` 6, item 3); the catalog checks
are the narrow, certain slice, not an attempt at what sqlx's `query!` gets from asking a real server.
A consumer who needs that keeps `query!` on its own pool (section 5, and board 230 option A).

## 3. The consumer API (question c)

### 3.1 Macro

```rust
use persistant::sql;                          // re-export of unisql_macros::sql
const FIND: persistant::Sql = sql!("SELECT id, email FROM users WHERE id = $1");
db.query_sql(&FIND, &[Value::Int(7)]).await?;
```

The source dialect is PostgreSQL (UniSQL's rule). The macro translates for every enabled target and
embeds each target's text, so there is no translation at run time and an untranslatable statement does
not build. `sql!` is usable in `const` position (the expansion is a `static`/`const` table of
`(target, text)` pairs plus the parameter count).

### 3.2 Where the schema comes from

`sql!` finds the schema snapshot, in order: an explicit `sql!(schema = "path", "...")` argument;
`UNISQL_SCHEMA` (set once in `.cargo/config.toml` `[env]`, so every macro use in the workspace agrees);
`persistant-schema.json` in the invoking crate's manifest directory. **Absent, only the database-free
checks run, and the macro says so once per build as a warning** (never silently; a consumer who
believes they have column checks must be able to tell they do not).

A proc macro cannot register its input file with cargo on stable, so the expansion includes
`const _: &[u8] = include_bytes!("<absolute path>");`: a changed snapshot rebuilds the crate.

### 3.3 Run time: the `Sql` value

`Sql` is a small `Copy` type (static table of target texts, parameter count). A new extension trait on
`RelationalBackend` and `RelationalTx`:

```rust
trait RelationalSqlExt {
    async fn execute_sql(&self, sql: &Sql, params: &[Value]) -> Result<u64, RelError>;
    async fn query_sql(&self, sql: &Sql, params: &[Value]) -> Result<Vec<Row>, RelError>;
}
```

It picks the text for `self.dialect()`; a dialect with no embedded text is `RelError::Unsupported`
(it can only happen when the backend's feature was not enabled for the macro). It does **not** change
the core trait, so no backend and no consumer is broken. `Sql` is defined by UniSQL if the macro's
expansion can only name types from `unisql`; otherwise by PersistAnt with the macro expanding to a
path PersistAnt re-exports. **Open with UniSQL**: they decide, we implement the trait either way.

### 3.4 Cargo features

On the `persistant` facade: `sql-macros` (off by default; pulls `unisql-macros`, a proc-macro crate,
so a consumer who does not want it pays nothing). Which targets are translated follows the facade's
backend features: `sqlx-driver` (PostgreSQL) plus the `sqlite` and `mysql` features of `persistant-sqlx`.
Cargo features unify across a build, so the target set is the union of what any crate asked for; the
macro accepts `sql!(targets = [postgres, sqlite], "...")` to narrow it deliberately, and a statement is
never silently translated for fewer targets than the consumer's backends. Pre-1.0, so new features and
re-exports are minor bumps; the facade's MSRV floor is not raised by this.

## 4. With `persistant-migrate` (question d)

New verb `persistant-migrate snapshot --dir migrations --dialect postgres [--out FILE]`
(source 1), `snapshot --scratch --url URL` (source 3), `snapshot --check` (either). It reuses
`load_dir`, so the files the snapshot describes are exactly the files `up` applies and the
checksum ledger covers; the snapshot's header records the migration versions and checksums it was
derived from, so a snapshot that is older than the directory is *detected*, not merely different
(`snapshot --check` says "stale: migration 37 is not in the snapshot", not a diff of JSON). Exit
codes continue the existing table. The `_sqlx_migrations` ledger table is never in the snapshot.
`up` does not write the snapshot (it connects to production; a build artifact must not depend on that).

## 5. Typed rows (question e): the honest road

Today a row is a `Vec<Value>` read by position (`rows[0].int(0)`). The question lightbulb and Humboldt
will ask is whether they get `query!`-style typed records. **Not yet, and not in the same way.**

- **Stage 1 and 2 (above):** no typing. Statements are checked; rows stay positional.
- **Stage 3, `FromRow`:** `#[derive(persistant::Row)] struct User { id: i64, email: String }` and
  `sql!(as User, "SELECT id, email FROM users")`. The macro checks selected plain columns by name,
  arity and class against the snapshot. A column that is an expression needs an annotation
  (`count(*) AS "n!: i64"`, the same convention as sqlx), because nothing in a transpiler can infer
  it. Types are the intersection the engines share (`i64`, `f64`, `bool`, `String`, bytes, `Option<_>`
  per nullability); anything finer (`Uuid`, `Decimal`, timestamps) is decoded by the existing audited
  decoders (`docs/DECODER-AUDIT.md`) to the nearest shared type and is a documented loss.
- **What stays out of reach, by design:** a record *inferred from a real server* for the statement as
  executed on each engine. sqlx gets that by asking one live server at compile time; a layer that runs
  one statement on several engines cannot, because the engines infer different types for the same
  text. A program that wants it keeps `sqlx::query!` on its own pool, which is Option A of board 230
  and loses nothing.

### `query!` and PersistAnt's transaction type (board 230 note: was unverified; verified 2026-10-18)

**Not usable today.** `PgTx`, `MysqlTx` and `SqliteTx` each wrap a private `sqlx::Transaction` and offer
no accessor (no `Deref`, `AsMut`, `into_inner` or `executor()` in `crates/persistant-sqlx/src` at
`4febf59`; the `RelationalTx` impl is the only way in). `sqlx::query!` needs an `Executor`, which a
`&mut PgConnection` is and a `PgTx` is not. So a transaction opened through PersistAnt (with its
`Isolation` and its lock handling) cannot run a `query!`. `PgBackend::pool()` hands out the concrete
pool, so `query!` on the pool works, as board 230 said. The fix is small and additive: a
`PgTx::connection(&mut self) -> &mut sqlx::PgConnection` (and the two siblings), recorded as row C6 in
`ROADMAP.md`. It is an escape hatch and says so in its docs; whatever runs through it bypasses
translation and the trait's errors. Not built here.

## 6. Order of work and what each step waits for

| Step | Owner | Waits for |
|---|---|---|
| 0. This document, ROADMAP rows | PersistAnt | nothing (this PR) |
| 1. `unisql-macros` first slice: `sql!` with translate, syntax, placeholder count, refusal | UniSQL | - |
| 2. Re-export `persistant::sql!`, `Sql`, `RelationalSqlExt`, feature `sql-macros`, compile-fail tests | PersistAnt | step 1 published to crates.io (portfolio rule: no path or git deps) |
| 3. `Schema` enumeration + serde + `Column` detail fields; the file format | UniSQL | agreement on section 1 |
| 4. Catalog readers in `persistant-sqlx`; `persistant-migrate snapshot` and `--check`; CI job | PersistAnt | step 3 |
| 5. Schema-reading checks in the macro | UniSQL | step 3 (writes), step 4 (real snapshots to test with) |
| 6. `FromRow` derive and `sql!(as T, ..)` | PersistAnt (derive) / UniSQL (check) | steps 4-5 |
| C6. `*Tx::connection()` accessor | PersistAnt | nothing; only if a consumer wants `query!` inside a PersistAnt transaction |

Tests that this works (rules: benchmarks and checks are tests; no `#[ignore]`): compile-fail tests (a
`trybuild`-style suite) with one failing statement per check in section 2 **and a passing twin for
each** (the control: the same statement with the fault removed builds); the snapshot of this repo's own
`rel.*` leaf tables checked against all three servers in the existing CI jobs; and `snapshot --check`
as a CI step.

## 7. Questions for the UniSQL lane (to settle before either side freezes)

1. Is the snapshot format UniSQL's (serde on `Schema`, PersistAnt writes it) or PersistAnt's with a
   converter? Recommendation: UniSQL's.
2. `Column` gains `declared_type`, `nullable`, `has_default`; `Schema` gains enumeration. Accepted?
3. `Sql`: defined in `unisql` or re-exported from here?
4. Where the macro looks for the schema (section 3.2), and the warning when none is found.
5. Target set: the macro's own cargo features, or `targets = [...]` only? Recommendation: both, with
   the argument winning.

## 8. Not decided here

Whether a Node or other-language binding gets the macro (it cannot; macros are Rust), and whether a
hosted mode ever serves checks. Out of scope.
