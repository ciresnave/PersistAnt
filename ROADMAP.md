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

1. **Durability verification.** *Done for `Process`* (0.5.8, `kv.durability.process`: a writer killed
   after acknowledged writes, the store reopened; SQLite file, OpenDAL fs, PostgreSQL, MySQL; memory
   stores fail it as they must). **`Machine` (power loss, an OS crash) is UNVERIFIED and stays
   declared**: a process kill leaves the OS and its caches intact. The harness for it needs a VM or a
   runner that can be hard-reset mid-write (write on a guest, power-cycle the guest, read back after
   boot, with the same known-bad control: a store that acknowledges before it flushes). Scope: kv on a
   file (SQLite, OpenDAL fs) and on a server database inside the guest. Not built, not scheduled; it
   needs infrastructure the PM provides. Options and a recommendation: `docs/DESIGN-DURABILITY-MACHINE.md`.
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
| Per-migration checksums | Built: ledger is sqlx's own (SHA-384 of the text), a changed applied migration is `ChecksumMismatch`; but `MigrateReport` lists versions only | **C1.** `MigrateReport` carries each applied migration's checksum |
| Fresh-install check | Not built | **C2.** `verify_fresh(&[Migration])`: apply to an empty schema, return the ledger, no side effects elsewhere |
| Load Humboldt's 36 `migrations/*.sql` | `Migration { version, description, sql }` built by hand | **C3.** a loader from `sqlx::migrate!` / a directory, so a consumer does not copy files into code |
| Row expiry sweep over its own tables | `kv.purge_expired` sweeps the kv table only | **C4.** `rel.expire(table, column, now)`: delete rows past a timestamp column; needs a decision (below) |
| 399 `sqlx::query!` calls, 29 transactions | `PgBackend::pool()` hands back the concrete `PgPool`; the relational trait is deliberately small | **none, by design (D1)**: typed queries stay with the consumer |
| Encrypted dump, per-table digest, restore check | Not built, no consumer outside Humboldt measured | **C5, only if a second consumer wants it** |

Open (CireSnave, via the PM): what "adopt" means for a project whose value is compile-time-checked SQL. Reading A (wrap
the pool, take migrations and expiry) needs C1-C4 only. Reading B (queries through the trait) removes the
compile-time check and is not offered. Recommendation: A. C4 also needs the owner of retention policy named: PersistAnt
provides the mechanism, the consumer names tables and ages.

Order if approved: C3, C1, C2 (one PR each, small), then C4. Nothing here is scheduled until the PM answers.

## SQLite and MySQL backends (through UniSQL)

`KvOnRelational` written once in PostgreSQL SQL and translated by UniSQL (`DESIGN-KINDS.md` §5).
Depends on UniSQL 0.24.4 or later from crates.io.
