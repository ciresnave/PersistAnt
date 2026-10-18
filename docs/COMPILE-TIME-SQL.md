# Compile-time SQL: re-exporting UniSQL's macros with catalog-based checks

Status: **design, 2026-10-18, not built.** Written against PersistAnt 0.8.3 (`4febf59`) and UniSQL 0.24.11
(`a5cb56e`). Origin: PM [TASK], quoting CireSnave (`CIRESNAVE-EXPECTATIONS.md` 6.16): *"have PersistAnt
re-export it and the catalog-based checks. ... All of our projects should strive to provide that same
'checks happen at compile time' functionality where applicable."*

UniSQL's own design is `docs/MACROS.md` (unisql#63: `unisql-macro-core`, `unisql-macros`, `unisql-embed`).
Their answers of 2026-10-18 are folded in below. The `Checker` seam (section 3.5) stays unstable until this
repo has built against it.

## 0. The split of work

| | UniSQL | PersistAnt |
|---|---|---|
| `unisql-macro-core` (parse input, translate per target, `Expansion`, the `Checker` trait), `unisql-macros` (`sql!`, thin), `unisql-embed` (the `Query` value) | **owns** | depends on core and embed |
| **A PersistAnt proc-macro crate** (`persistant-macros`): a proc-macro crate cannot be extended, only wrapped, so this repo writes its own thin one that calls `unisql-macro-core` with a catalog `Checker` | - | **owns**; the facade re-exports it |
| `Schema` type, a reference `Checker` over it (names only), `schema_from_ddl` | **owns** | uses; implements the catalog `Checker` on top |
| Schema snapshot **file format** (serde on `Schema` behind a feature, a `format` integer) | **owns** | writes it |
| Producing a snapshot: from migration files, from a live catalog | `schema_from_ddl` exists | **owns** the catalog readers (in `persistant-sqlx`) and `persistant-migrate snapshot` |
| Running an embedded statement on a backend | `Query` (const-constructible, per-target text) | aliases it as `Sql`; **owns** `RelationalSqlExt` (section 3.3) |

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
use persistant::sql;                          // PersistAnt's macro (wraps unisql-macro-core)
const FIND: persistant::Sql = sql!(
    source = postgres, targets = [postgres, sqlite, mysql],
    "SELECT id, email FROM users WHERE id = $1"
);
db.query_sql(&FIND, &[Value::Int(7)]).await?;
```

The call names `source` and `targets` as UniSQL's macro requires: no hidden target set, because cargo
features unify across a build. A project that wants a default writes a one-line `macro_rules!` wrapper
of its own. The macro translates for every named target and embeds each target's text, so there is no
translation at run time and an untranslatable statement does not build. All of UniSQL's options
(`allow = [..]`, `requires`, `forbids`) pass through.

### 3.2 Where the schema comes from

Resolved by **this** macro before it calls core (UniSQL's macro reads no environment, so that its
expansion is pure for rust-analyzer and cannot go stale): a `schema = "path"` argument, else
`persistant-schema.json` in `CARGO_MANIFEST_DIR`. A `.sql` path is DDL in the source dialect (UniSQL's
own `schema =` form), a `.json` path is the snapshot of section 1. **If neither exists it is a compile
error**, not a quiet downgrade: the message says to run `persistant-migrate snapshot` or to write
`schema = none` for database-free checks only. That keeps the rule that a consumer who believes they
have column checks can tell when they do not, without needing a warning (stable proc macros cannot emit
one). The expansion embeds the file with `include_bytes!`, so changing it rebuilds the crate.

### 3.3 Run time: the `Sql` value

`pub type Sql = unisql_embed::Query;`, with the expansion path overridden (`crate = ::persistant::embed`,
which UniSQL's macro supports) so a consumer depends on neither UniSQL crate. A new extension trait on
`RelationalBackend` and `RelationalTx`:

```rust
trait RelationalSqlExt {
    async fn execute_sql(&self, sql: &Sql, params: &[Value]) -> Result<u64, RelError>;
    async fn query_sql(&self, sql: &Sql, params: &[Value]) -> Result<Vec<Row>, RelError>;
}
```

It picks the rendered text for `self.dialect()` and applies the rendering's `param_order` (a target may
reorder or repeat placeholders). A dialect with no embedded text is `RelError::Unsupported` naming the
dialect (the call did not list it in `targets`; UniSQL's open question would make that a compile
error). The core trait does not change, so no backend and no consumer is broken.

### 3.4 Cargo features

On the `persistant` facade: `sql-macros` (off by default; pulls `persistant-macros` and
`unisql-macro-core`, so a consumer who does not want a proc-macro build pays nothing). UniSQL's dialect
features only decide which dialect crates are compiled into the macro, a build-cost knob; naming a
target whose feature is off is a build error. Pre-1.0, so the new feature, crate and re-exports are
minor bumps; the facade's MSRV floor is not raised by this.

### 3.5 The `Checker` seam

`unisql-macro-core` calls `Checker::check(&ast::Statement, &CheckContext) -> Vec<Problem>` on the parsed
source statement before translation; each problem becomes a compile error at the call. PersistAnt's
checker loads the snapshot into a `unisql::Schema`, runs UniSQL's reference checker (names), and adds the
checks that need the richer snapshot fields (arity, `NOT NULL` without default, classes, later types).
The trait is unstable until built against; this repo is its first consumer and settles it in UniSQL's
slice 4 and step 3 below.

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

UniSQL's slices (`MACROS.md` section 8): 1 core and embed, 2 bare `sql!` with `source`/`targets` and a
`trybuild` suite, 3 `allow`/`requires`/`forbids` and the argument-count form, 4 `schema =` and the
reference `Checker`, 5 caching and a measured build cost.

| Step | Owner | Waits for |
|---|---|---|
| 0. This document, ROADMAP rows | PersistAnt | nothing (this PR) |
| 1. UniSQL slices 1-2, published (PM claims the three crate names first) | UniSQL | - |
| 2. `persistant-macros` (bare wrapper over core), `Sql` alias, `RelationalSqlExt`, feature `sql-macros`, compile-fail tests | PersistAnt | step 1 on crates.io (portfolio rule: no path or git deps) |
| 3. `Schema` serde and enumeration, `Column` `declared_type`/`nullable`/`has_default`; the `Checker` seam | UniSQL (slice 4) | agreement on section 1, this repo as first consumer |
| 4. Catalog readers in `persistant-sqlx`; `persistant-migrate snapshot` and `--check`; CI job | PersistAnt | step 3 |
| 5. Catalog `Checker` and schema lookup in `persistant-macros` | PersistAnt | steps 3, 4 |
| 6. `FromRow` derive and `sql!(as T, ..)` | PersistAnt | step 5 |
| C6. `*Tx::connection()` accessor | PersistAnt | nothing; only if a consumer wants `query!` inside a PersistAnt transaction |

Tests (rules: checks are tests; no `#[ignore]`): a `trybuild` compile-fail suite with one failing
statement per check in section 2 **and a passing twin for each** (the control: the same statement with
the fault removed builds). `.stderr` files are rustc-version sensitive, so CI pins the toolchain for
that test, as UniSQL does. Also: the snapshot of this repo's own `rel.*` leaf tables checked against
all three servers in the existing CI jobs, and `snapshot --check` as a CI step.

## 7. Settled with the UniSQL lane (2026-10-18) and what is still open

Settled: the snapshot format is UniSQL's and this repo writes it; `Schema` gains enumeration and serde
(behind a feature) and `Column` gains `declared_type`, `nullable`, `has_default` (additive); `Sql` is
`unisql_embed::Query`; no environment lookup in UniSQL's macro; `targets = [..]` required at each call.

Open: the exact `Checker`/`CheckContext` shape (settled when built); whether asking for a target that
was not embedded can be a compile error (UniSQL's open question); a project-wide default `targets` (a
`macro_rules!` wrapper today).

## 8. Not decided here

Whether a Node or other-language binding gets the macro (it cannot; macros are Rust), and whether a
hosted mode ever serves checks. Out of scope.
