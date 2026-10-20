# PersistAnt roadmap

Priorities live in the portfolio roadmap; this file holds the rows this repo owns. Design: `docs/DESIGN-KINDS.md`.

## Backend benchmarking and ranking

Approved by CireSnave 2026-10-16: *"I agree with your thinking about PersistAnt's benchmarking of backends.  Get it on the correct roadmaps and lets get it built."*

One benchmark per benchmarkable capability, so PersistAnt can rank backends and a consumer can ask for
the best backend for a capability. Four stages, each its own PR, in order:

1. **Capability contract suite, per kind (kv first).** Pass/fail only. A backend that fails a contract
   is **excluded**, never ranked slower. The model is Synapse's `tests/mailbox_suite`. Motive: Synapse's
   spike found `run_all` passes `MemoryKv` and fails `KvOnBlob` (no `batch`; 0 versus 74/100 duplicate
   sequence numbers with and without `Require`).
2. **Precision and per-operation statistics.** Errors, ordering and durability checks, per operation.
3. **Speed.** The environment is recorded with every result (machine, backend version, date) together
   with a re-run command. No constant rankings.
4. **`best backend for X` API.** Returns a **ranked list with the evidence**. The consumer states the
   capabilities it needs and a minimum durability class; the choice is explicit and logged, never silent.

**Status (kv as of 0.5.6, 2026-10-16; main is 0.7.8).** All four stages are done for the **kv** kind:
`contract::check` (#39, with the Synapse duplicate-sequence workload as a leaf, #40), `stats::measure`
(#43), `speed` with data files in `bench/` (#46), `choose::choose` (#47). Stage 2 also exists for
blob (#45). Not done, in this order (queued by the PM 2026-10-16):

1. **Durability verification.** *`Machine`: **done for guest power loss on ext4 (Hyper-V)**, run 39178436801 on main 02f86a7 (`scripts/machine-durability/`): kv-on-relational on SQLite `synchronous=FULL`, PostgreSQL and MySQL held every acknowledged key in 5 of 5 power cuts each; the `synchronous=OFF` control lost in 4 of 4; SQLite disk-full failed loud with its acknowledged keys kept. Marked `Declared.verified = Machine` for those three only (0.8.12; for SQLite, since 0.8.14, only the `sqlite-full` profile). UNVERIFIED and stays so: host power loss, NTFS, the disk's own cache, torn sectors, remote object stores. **Disk full (ENOSPC), guest ext4, run 39325665814 on main 9a9fad2** (harness 0.8.15-0.8.18): SQLite FAILS LOUD (330 acknowledged kept); PostgreSQL FAILS LOUD (a put returns `No space left on device` from `pg_wal`; 21,496 acknowledged, all kept; the server can PANIC and stays down until about 10 MB is freed and it is restarted); **MySQL STALLS, it does not fail loud** (InnoDB logs `Disk is full ... Waiting for someone to free space` and the client gets no answer; 7,118 acknowledged, all kept once space was freed), so a caller of MySQL needs its own timeout. NTFS, host and remote stores untested.* Approved by CireSnave 2026-10-18 (board 229: A, Hyper-V). *Done for `Process`* (0.5.8, `kv.durability.process`: a writer killed
   after acknowledged writes, the store reopened; SQLite file, OpenDAL fs, PostgreSQL, MySQL; memory
   stores fail it as they must). **`Machine` (power loss, an OS crash) is UNVERIFIED and stays
   declared**: a process kill leaves the OS and its caches intact. The harness for it needs a VM or a
   runner that can be hard-reset mid-write (write on a guest, power-cycle the guest, read back after
   boot, with the same known-bad control: a store that acknowledges before it flushes). Scope: kv on a
   file (SQLite, OpenDAL fs) and on a server database inside the guest. Not built, not scheduled; it
   needs infrastructure the PM provides. Options and a recommendation: `docs/DESIGN-DURABILITY-MACHINE.md`.
1a. **SQLite declared class.** An explicit `synchronous=FULL` profile declares `Machine` (what Synapse needs per board 229: A, now backed by run 39178436801); the default stays `Process`. **Built (0.8.14):** `SqliteBackend::open_durable`, backend name `sqlite-full`.
1b. **`kv.bounded_call` (built, 0.8.20).** An optional per-call timeout (`persistant_sqlx::Bounded`, default none); the leaf is declared only when one is configured. Why: MySQL stalls on a full disk and a caller that waits forever is worse than one that is told (Synapse requires a bound).
2. **`choose` for the blob kind** *(done, 0.6.0: contract, speed and choose on the shared core types;
   blob durability is declared and unverified)*, then **the relational kind**. *Its first leaves are
   done (0.6.1: `rel.values`, `rel.transaction`, `rel.error.unique`, `rel.foreign_key`,
   `rel.check_constraint`; on for PostgreSQL, SQLite and MySQL, with a weaker positive control than kv
   and blob: stated in the code); then `rel.isolation.*` (no_dirty_read, repeatable_read, serializable;
   0.7.0 made `begin` take an `Isolation`), `rel.savepoint`, `rel.row_lock`, `rel.upsert_where`; then
   contract, speed and `choose` for it (0.7.1: four workloads, durability declared and unverified).*
   Migrations as leaves are built (0.7.2, five `rel.migrate.*` leaves; MySQL cannot undo DDL; `rel.upsert_where` runs on MySQL as `ON DUPLICATE KEY UPDATE .. IF(..)` since 0.7.5). Relational speed workloads for `rel.savepoint` and `rel.upsert_where` are built (0.7.3). Concurrent migrators are a leaf (`rel.migrate.concurrent`, 0.7.4; MariaDB 0.7.7). Owner: the PersistAnt lane.
3. **Synapse `tests/mailbox_suite` as a stage-1 consumer check**: the Synapse lane runs `run_all` on
   each eligible backend (Postgres, SQLite, MySQL) and reports which leaves it adds.
4. Not covered by any stage yet: concurrency-aware speed, value sizes other than 256 bytes, ranking
   by more than one workload.

Rules: benchmarks are tests. They run in CI or on the self-hosted runner, fail loudly on regression, and
use no `#[ignore]` and no `allow(dead_code)`. Detector: the contract suite goes red for a backend that
regresses.

## Adoption candidates: Humboldt (what a concrete-pool consumer needs)

Source: Humboldt `docs/ADOPTION-PLAN.md` at `8e1a304` (branch `docs/adoption-plan`, not yet on its main), section 2,
read 2026-10-17. Ruling it cites (board 196, relayed by the PM): all projects adopt PersistAnt for persistence.
**That plan describes PersistAnt at 0.4.32 (`5812cb7`); main is 0.7.9, so two of its gaps are already closed:**

| Humboldt asks for | PersistAnt at 0.7.9 | Candidate |
|---|---|---|
| Advisory-lock-guarded migration apply | Built: `Migrates::migrate` holds the lock; `rel.migrate.concurrent` proves two simultaneous migrators (PostgreSQL, MySQL, MariaDB) | none |
| Per-migration checksums | Built: ledger is sqlx's own (SHA-384 of the text), a changed applied migration is `ChecksumMismatch`; but `MigrateReport` lists versions only | **C1, built (0.8.0):** `MigrateReport.checksums` carries each applied migration's checksum |
| Fresh-install check | Not built | **C2.** `verify_fresh(&[Migration])`: apply to an empty schema, return the ledger, no side effects elsewhere |
| Load Humboldt's 36 `migrations/*.sql` | `Migration { version, description, sql }` built by hand | **C3.** a loader from `sqlx::migrate!` / a directory, so a consumer does not copy files into code |
| Row expiry sweep over its own tables | `kv.purge_expired` sweeps the kv table only | **C4, built (0.7.13):** `persistant_relational::sweep(db, table, column, cutoff, batch, max_batches)`; the consumer owns the retention policy (PM, 2026-10-17) |
| 399 `sqlx::query!` calls, 29 transactions | `PgBackend::pool()` hands back the concrete `PgPool`; the relational trait is deliberately small | **none, by design (D1)**: typed queries stay with the consumer |
| Encrypted dump, per-table digest, restore check | Not built, no consumer outside Humboldt measured | **C5, only if a second consumer wants it** |

Open (CireSnave, via the PM): what "adopt" means for a project whose value is compile-time-checked SQL. Reading A (wrap
the pool, take migrations and expiry) needs C1-C4 only. Reading B (queries through the trait) removes the
compile-time check and is not offered. Recommendation: A. C4 also needs the owner of retention policy named: PersistAnt
provides the mechanism, the consumer names tables and ages.

Order if approved: C3, C1, C2 (one PR each, small), then C4. Nothing here is scheduled until the PM answers.

## Migration CLI (queued by the PM 2026-10-17; first cut built, 0.8.2)

A command-line tool over `Migrates`: apply, status (ledger with checksums, C1) and fresh-install check
(`verify_fresh`, C2) for a directory of `NNN_name.up.sql` / `NNN_name.down.sql` files, loaded through
`migrations_from_sqlx` (C3). Scope to settle before building:

- **Down files.** `Migration` and `Migrates` have no undo today; `rel.migrate.*` pins that MySQL cannot undo DDL. A `down`
  verb is a new trait method and leaf, so the first cut is **up, status and verify-fresh only**; `.down.sql` files are read and
  checked for pairing, never run.
- **Shape.** A new binary crate in this workspace, one binary for PostgreSQL, SQLite, MySQL and MariaDB by URL scheme; exit
  codes distinguish `ChecksumMismatch`, `Dirty` and "database newer than the program".
- **Built:** crate `persistant-migrate` (`up`, `status`, `verify-fresh` on all four databases, 0.8.3). Tested end to end on SQLite, PostgreSQL and MySQL/MariaDB in CI. A target-printing, `--allow-remote` guard refuses non-local targets (exit 9), after the Community 0033 incident (PM, 2026-10-17).
- **Node binding** is later and out of this row; **hosted mode** is later still and not designed here.

Not scheduled until the PM assigns it.

## Compile-time SQL checks (PM [TASK] 2026-10-18)

CireSnave 2026-10-18 (`CIRESNAVE-EXPECTATIONS.md` 6.16): *"have PersistAnt re-export it and the catalog-based checks. ... All of our projects should strive to provide that same 'checks happen at compile time' functionality where applicable."* Design: `docs/COMPILE-TIME-SQL.md` (not built; interface with the UniSQL lane not yet frozen). Rows, in order:

1. **Design agreed with UniSQL** (their `Schema` serde and `Column` detail fields, `Sql` location, schema lookup). *Open.*
2. **`persistant-macros` (a thin proc-macro over `unisql-macro-core`, UniSQL cannot be extended in place) and `persistant::sql!`** plus `Sql` and `RelationalSqlExt`, feature `sql-macros`, compile-fail tests. *Built, 0.8.21 (UniSQL 0.24.13 is on crates.io). No schema yet: see rows 3-5.*
3. **Catalog readers** (`persistant_relational::catalog`, over the `RelationalBackend` trait: PostgreSQL, MySQL/MariaDB, SQLite) and **`persistant-migrate snapshot` / `--check`**. *Built, 0.8.22: writes UniSQL's `Snapshot` (unisql-core 0.24.17) from a live catalog or a scratch apply. Not built: a database-free source from the migration files (`schema_from_ddl`). A CI step in a consumer's repo is theirs to add.*
   *Row 5 (the catalog `Checker` in `persistant-macros`, `sql!` reading the snapshot): built, 0.9.0, with `schema = none` as the stated opt-out; checks: unknown columns and `INSERT` arity (UniSQL's), a `NOT NULL` column without default left out, `ON CONFLICT` not a unique key. Operand classes and argument/row types are row 6.*
4. **`FromRow` derive and `sql!(as T, ..)`** (typed rows over the shared value classes). *Waits for 2 and 3.*
5. **C6 (optional): `PgTx` / `MysqlTx` / `SqliteTx` `connection()` accessors**, so `sqlx::query!` can run inside a PersistAnt transaction. Verified missing at `4febf59`; additive; built only if a consumer asks (board 230 option A uses the pool, which already works).

## SQLite and MySQL backends (through UniSQL)

`KvOnRelational` written once in PostgreSQL SQL and translated by UniSQL (`DESIGN-KINDS.md` §5).
Depends on UniSQL 0.24.4 or later from crates.io.
