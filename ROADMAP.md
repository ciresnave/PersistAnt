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

**Status (kv as of 0.5.6, 2026-10-16; main was 0.7.8 then).** All four stages are done for the **kv** kind:
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
4. Not covered by any stage yet: concurrency-aware speed: harness built (0.11.0, `run_concurrent`, example
   `speed_concurrent`, host load and RAM recorded at start and end of every result); the timing runs wait for a PM quiet window. Ranking by more than one workload: built (0.10.5,
   `compare_across`, no composite score). Value sizes: code built (0.10.3, `run_sized`); the sized data files wait for a quiet machine.

Rules: benchmarks are tests. They run in CI or on the self-hosted runner, fail loudly on regression, and
use no `#[ignore]` and no `allow(dead_code)`. Detector: the contract suite goes red for a backend that
regresses.

## Adoption candidates: Humboldt (what a concrete-pool consumer needs)

Source: Humboldt `docs/ADOPTION-PLAN.md` at `8e1a304` (branch `docs/adoption-plan`, not yet on its main), section 2,
read 2026-10-17. Ruling it cites (board 196, relayed by the PM): all projects adopt PersistAnt for persistence.
**That plan describes PersistAnt at 0.4.32 (`5812cb7`); main was 0.7.9 when read, so two of its gaps are already closed:**

| Humboldt asks for | PersistAnt at 0.7.9 | Candidate |
|---|---|---|
| Advisory-lock-guarded migration apply | Built: `Migrates::migrate` holds the lock; `rel.migrate.concurrent` proves two simultaneous migrators (PostgreSQL, MySQL, MariaDB) | none |
| Per-migration checksums | Built: ledger is sqlx's own (SHA-384 of the text), a changed applied migration is `ChecksumMismatch`; but `MigrateReport` lists versions only | **C1, built (0.8.0):** `MigrateReport.checksums` carries each applied migration's checksum |
| Fresh-install check | Built | **C2, built (0.8.x):** `verify_fresh(&[Migration])`: apply to an empty schema, return the ledger, no side effects elsewhere; `persistant-migrate verify-fresh` |
| Load Humboldt's 36 `migrations/*.sql` | Built | **C3, built (0.8.x):** `migrations_from_sqlx` and the directory loader of `persistant-migrate`, so a consumer does not copy files into code |
| Row expiry sweep over its own tables | `kv.purge_expired` sweeps the kv table only | **C4, built (0.7.13):** `persistant_relational::sweep(db, table, column, cutoff, batch, max_batches)`; the consumer owns the retention policy (PM, 2026-10-17) |
| 399 `sqlx::query!` calls, 29 transactions | `PgBackend::pool()` hands back the concrete `PgPool`; the relational trait is deliberately small | **Board 230, ruled option 3 (CireSnave 2026-10-22): the sqlx bridge**, see the section below. Typed queries keep sqlx's own check |
| Encrypted dump, per-table digest, restore check | Not built, no consumer outside Humboldt measured | **C5, only if a second consumer wants it** |

Open (CireSnave, via the PM): what "adopt" means for a project whose value is compile-time-checked SQL. Reading A (wrap
the pool, take migrations and expiry) needs C1-C4 only. Reading B (queries through the trait) removes the
compile-time check and is not offered. Recommendation: A. C4 also needs the owner of retention policy named: PersistAnt
provides the mechanism, the consumer names tables and ages.

C1 to C4 are built (above). Reading A is what Humboldt takes; C5 stays "only if a second consumer wants it".

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

## The sqlx bridge (board 230 option 3; design `docs/DESIGN-SQLX-BRIDGE.md`)

CireSnave 2026-10-22: *"I like your option (3) that keeps the SQLx PostgreSQL checks in place but transitions everything to use the resulting SQL through UniSQL/PersistAnt."* Work orders, each a PR:

- **A, built (0.9.14):** `FromValue` / `ToValue` for `Uuid`, `chrono` and `serde_json::Value` (features `sql-uuid`, `sql-chrono`, `sql-json`).
- **B, built (0.9.15):** the reader for sqlx's offline data (`.sqlx/query-<sha256>.json`), checked against all 566 of Humboldt's files.
- **C, built (0.9.17):** `sqlx_query!`, `sqlx_query_as!`, `sqlx_query_scalar!` (feature `sqlx-query`; `sqlx-query-checked` also runs sqlx's own check in a closure that is never called), `Bound`, `Runner`, `InTx`.
- **D, built (0.9.18):** `persistant_sqlx::OnPool` / `OnConn`, so the macros run on the sqlx pool or transaction a program already holds.
- **E, not scheduled (PM 2026-10-23: skip):** name checks for `SELECT` / `DELETE` in our catalog checker. Its value is for consumers that do not have sqlx's check; Humboldt does.
- **F:** the Humboldt option-A items (migration loader, fresh-install verifier, expiry sweeps) were already built (C2 to C4 above).
- UniSQL asks (`LATERAL`, `::regclass`) landed in UniSQL 0.25.1; 0.25.8 (0.9.22) fixes the SQLite rowid-alias false positive.

## Key-value additions for auth-framework (PM [TASK] 2026-10-23; design `docs/DESIGN-AUTH-STORAGE.md`)

Built and published as 0.9.24: `KvBackend::take` (`kv.take`, 0.9.20, over `compare_and_delete`), `KvBackend::count` (`kv.count`, 0.9.23, native on memory and the SQL engines) and `persistant_kv::index::Indexed` (`kv.index`, 0.9.24, secondary-key lookup over `batch` and `ordered_scan_values`). On memory, SQLite, PostgreSQL, MySQL and MariaDB; **not** on the OpenDAL blob stores, which have neither `compare_and_delete` nor `batch`. The auth-framework lane builds its `AuthStorage` on them (its work, not this repo's). Open: Redis is not a PersistAnt backend; throughput of the index layer against auth-framework's own SQL is unmeasured.

## Dependencies behind latest

CireSnave's rule is every dependency on its latest version. Two are not, and neither is held by our manifests (`cargo update --dry-run --verbose` at `743bda7`, 2026-10-30). Each row has a blocker and the release that ends it; re-run the dry run at every dependency PR and delete a row when its trigger fires.

| Dependency | In the lock | Latest | Blocker | Revisit when |
|---|---|---|---|---|
| `crypto-common` | 0.1.6 | 0.1.7 | 0.1.7 would downgrade `generic-array` 0.14.9 to 0.14.7 (`cargo update -p crypto-common --precise 0.1.7`) | a `crypto-common` 0.1.x that no longer pins `generic-array` below 0.14.9, or `digest` 0.10.x leaving the tree |
| `sha2` | 0.10.9 (a second copy; ours is 0.11.1) | 0.11.1 | `sqlx-core` 0.9.0 depends on `sha2` 0.10, and 0.9.0 is the latest `sqlx` | an `sqlx` release that depends on `sha2` 0.11 |

## Portfolio adoption (survey: `docs/CONSUMERS.md`, 2026-10-23)

CireSnave's standing order (2026-10-21, `CIRESNAVE-EXPECTATIONS.md` 6.17): no project is limited to one backend where PersistAnt offers several. The survey read twelve projects; adoption order: auth-framework, commy, agentlife, lightbulb, synapse, Humboldt (five are not candidates; mlmf waits on streamed blob I/O). Those are the lanes' own work. What it asks of this repo, in order of how many projects each unblocks; none is scheduled until the PM assigns it:

| # | Ask | State |
|---|---|---|
| 1 | A registry example and a short guide for projects that still name a backend in code (`Store` + `Config::Fs`: commy, agentlife, lightbulb) | **built (0.9.31)**: `docs/REGISTRY-FOR-STORE-USERS.md` and `examples/registry_instead_of_config.rs`; what does not map (`CreateIfAbsent` on `fs`, append, locks, a blocking call) is stated there |
| 2 | A matrix row for `lock.process` | **built (0.9.27)**: `std-file-lock`, decided by a killed child process |
| 3 | Pool options on `PgBackend::connect` (lightbulb builds its own pool for an acquire timeout) | **closed, nothing to build** (checked 2026-10-23): `PgBackend::connect_with(&ConnectionSecret, &ConnectOptions { tls, max_connections, acquire_timeout })` has existed since 0.4.31; lightbulb's comment is stale. Its default TLS policy is `Require` |
| 4 | Streamed or ranged blob I/O (`blob` is whole-value today) | design merged (0.12.4, `docs/DESIGN-BLOB-STREAMS.md`; mlmf answered: no mmap, no resume, `fs` only). **Work orders 1 (0.12.7: trait, `MemoryBlob`, four conformance leaves with controls, `ChunkReader`/`copy_in`, wrappers) and 2 (0.12.8: the OpenDAL driver) built.** **Work order 3 step 1 built (0.13.2, PR #181): cross-process staging, `fs` streamed writes staged by hand with a lock, cleaned at open.** **Work order 3 built: 3b crash test 4b (0.13.4, PR #184), 3d bounded-memory test (0.13.5, #185), 3c old-value 4a (0.13.6, #186), 3e the `DESIGN-KINDS.md` ids (0.13.7).** **Work order 4 built (0.14.1, #190): blocking `BlobHandle` `read_range` / `read_range_if` / `write_stream[_atomic]` with `BlobWriter`, docs, example.** Also: `Matrix::lookup` answered by `capant::resolve` (0.13.8, #188); the gate no longer calls a probe-less `yes` On (0.14.0, Breaking, #189). **Work order 5 built (0.14.3): patent re-sweep recorded in `docs/PATENT-SWEEP.md`: outside as read, not a clearance; 13 patents unread; one arguable-low (US9117027B2); web search only, claim 1 quoted per patent.** Not a green light by itself: the publish (PM) follows the gate |
| 5 | `storage.no_plaintext_at_rest` (reserved in the vocabulary: a canary value written through the layer is not found as raw bytes in the data directory, log or temp files) | **tested for `opendal-fs` (0.16.6):** the canary is found raw, so the row is off (`missing`); on only for a layer or volume that really encrypts. SQLite files not yet covered; no project states the need yet |
| 6 | A Redis backend | only if auth-framework's operators need it |
| 7 | Owner-only files on Windows | **built (0.9.43)** for SQLite databases and **(0.15.0)** for `fs` blob files created by an `owner_only()` driver (see File permissions) |
| 8 | CodeRipper OPS-011 `cli-basics` for `persistant-migrate` (PM [TASK] 2026-10-29: it failed 4 of 5; `--version`/`--help` were usage errors) | **built (0.12.6)**: flags are read before any other parsing, `--help`/`-h` and `--version`/`-V` exit 0, an unknown flag or subcommand exits 2 before any connection; `crates/persistant-migrate/tests/cli_basics.rs` |
| 8 | **The registry's `fs` had `kv.put_if_absent` off** (found 2026-10-23 by running the guide's example): its `atomic_write_dir=set` configuration made the exclusive create check-then-rename (`matrix/opendal.tsv`), so a program that needs CreateIfAbsent was offered only the SQL backends. **Built (0.9.41; PM [TASK] 2026-10-24):** under a scratch directory the driver writes the whole value to a scratch file and `hard_link`s it to the key (atomic, fails if the key exists, value whole when visible). `crates/persistant-opendal/tests/fs_create_exclusive.rs` runs 16 contenders x 30 rounds on real threads, with the old check-then-write as a negative control (every contender wins). It needs the scratch directory on the root's filesystem and hard links there (else `Unsupported`, recorded off). `Store::create_if_absent` (the old facade) still refuses `atomic_write_dir` | built (0.9.41) |
| 9 | **The registry's `fs` aliased keys that differ only by case on a case-insensitive filesystem** (measured on Windows/NTFS 2026-10-23: put `Key`=upper, put `key`=lower, get `Key` returned lower). 0.9.33 refused such a directory (`OpenError::CaseInsensitiveFilesystem`). **Built (0.9.42; PM [TASK] 2026-10-24):** `BlobBackend::key_layout` (`Raw` default; `fs` says `HexPath`) and `KvOnBlob` writes a `HexPath` store's keys as lower-case hex in path segments of at most 100 characters under `kvx/`, ending in a `-` segment that sorts before every hex digit. No two keys share a file whatever the filesystem does with case, separators, dots or reserved names, no key is another's directory, and bytewise order and prefixes are kept (`ordered_scan`, `prefix_list` unchanged). Tests: round trip, no collision (also ignoring case) and no key-as-directory over awkward keys and the 50-byte segment boundary, order and prefix equivalence, a case-folding store as the control (raw aliases, hex does not), and on a real directory (this machine's NTFS) every awkward key is its own value and lists in order. A directory written by 0.9.41 or earlier (`kv/<key>`) is moved at open (`KvOnBlob::migrate_raw_layout`, an exclusive copy then delete; tested). `open_named` no longer returns `CaseInsensitiveFilesystem` (the variant stays) | built (0.9.42) |

## CodeRipper baseline (advisory; first runs 2026-10-23)

The advisory `portfolio` job (0.9.34) swept this repository: run 40005822738 at coderipper `c665210a` (CodeRipper 0.8.19), then run 40031853264 at `a48fc6cc`, which sweeps each workspace member (the first pin analysed only the root package). Eight rules reported; **one row per rule**. The job skips `ci-protection-presence` (it needs repository-admin read); the PM's sweep reports that one. To reproduce locally: `coderipper sweep --profile portfolio --deny none --no-config --skip-rule ci-protection-presence=x --project <crate dir>` for the root and for each `crates/*`.

| Rule | Where | What it said, and what was done | State |
|---|---|---|---|
| `reachability` | `VERSION` (root); `is_under`, `rank`, `SEED`, `seed` (`persistant-core`); `count_prefix_paged`, `is_eligible`, `precision` (`persistant-kv`); `call_bound_from_env` (`persistant-sqlx`) | Nine public items nothing inside their crate calls. Nothing in the portfolio calls them either (eight repositories, `git grep` at `origin/main`). Removing a public item is a breaking change (0.10, and every matrix version column), so each is **declared public API** with a reason and owner in a `.coderipper.toml` (root file for the root package, one inside each member). The misplaced `VERSION` doc comment was fixed | resolved (0.9.38); revisit at the next breaking release |
| `lint-suppression` | `sqlx_bridge.rs:29,66`, `registry.rs:270,375` | Four `allow` attributes without a reason: each has one now | resolved (0.9.38) |
| `unused-return-values` | `persistant-macros` `ok`, `ok_in` (test helpers) | Their return values were discarded at 10 of 13 call sites: the value-returning helpers are `expansion` / `expansion_in`, `ok` / `ok_in` return nothing | resolved (0.9.38) |
| `SUP-001` | `persistant-migrate` | A binary with no committed `Cargo.lock`: committed, CI builds `--locked` | resolved (0.9.36) |
| `SUP-004` | five `checked_in_matrix*` tests | `include_str!` outside the crate: the tests moved to the root crate, one copy of each TSV | resolved (0.9.35) |
| `ci-security` | `machine-durability.yml`, `synapse-power-cut.yml` | The self-hosted power-loss jobs "mention no GPU": by design (the GPU is the rule's proxy for "needs our hardware"); waived in `.coderipper.toml` with a reason and owner | waived (0.9.37) |
| `licence` | `LICENSE-MIT` | The copyright holder was "CireSnave", which the rule reads as a person's name. CireSnave ruled (board item 263): the holder is "Eric Evans" | resolved (0.9.39) |

## File permissions

SQLite databases are created owner-only (`0600`) on Unix since 0.9.21 (a new test fails without it). **Windows, built (0.9.43; PM [TASK] 2026-10-24):** a new database file gets a *protected* DACL (nothing inherited from the directory) with one entry, full control for the user the process runs as, set through the Win32 security API (`windows-sys`, Windows only; `crates/persistant-sqlx/src/owner_only.rs`). `SYSTEM` and the administrators are not listed (as root is not on a `0600` file; an administrator can still take ownership). If the file cannot be restricted the open fails and the new file is removed; nothing is created open. `open_durable` restricts the `-wal` and `-shm` files too, because SQLite does not copy a Windows ACL as it copies a Unix mode. **Not covered:** the rollback `-journal` of `open` (created and removed by SQLite on every transaction) inherits the directory's ACL, so restrict the directory as well if journal contents matter, or use `open_durable`; and a file that already exists keeps its ACL, as on Unix. `crates/persistant-sqlx/tests/sqlite_file_acl.rs` reads the ACL back with `icacls` (a different route from the one that sets it), with a control that an ordinary file is not owner-only by the same reading and a mutation check (remove the call and the tests fail). It is a small Windows-only module in the sqlx driver, not a new OS-abstraction crate (the scope question the PM put to CireSnave is not needed for it). **Blob files (0.15.0):** `OpendalBlob::owner_only()` makes every file the `fs` blob driver creates owner-only the same way (`0600`/`0700` on Unix; the same protected ACL on Windows, code in `persistant_core::owner_only`), created that way before any content is written; a file that already exists keeps its mode; Windows directories are not restricted. `blob.file_mode` is recorded in `matrix/opendal.tsv` under `file_mode=owner_only`, read back by mode bits (Unix) or `icacls` (Windows); a driver not asked for it is not claimed to have it.

## Compile-time SQL checks (PM [TASK] 2026-10-18)

CireSnave 2026-10-18 (`CIRESNAVE-EXPECTATIONS.md` 6.16): *"have PersistAnt re-export it and the catalog-based checks. ... All of our projects should strive to provide that same 'checks happen at compile time' functionality where applicable."* Design: `docs/COMPILE-TIME-SQL.md` (not built; interface with the UniSQL lane not yet frozen). Rows, in order:

1. **Design agreed with UniSQL** (their `Schema` serde and `Column` detail fields, `Sql` location, schema lookup). *Done, 0.9.46: frozen in `docs/COMPILE-TIME-SQL.md` section 7 (stable within a UniSQL `0.n` line; breaks announced by `[NOTICE]`).*
2. **`persistant-macros` (a thin proc-macro over `unisql-macro-core`, UniSQL cannot be extended in place) and `persistant::sql!`** plus `Sql` and `RelationalSqlExt`, feature `sql-macros`, compile-fail tests. *Built, 0.8.21 (UniSQL 0.24.13 is on crates.io). No schema yet: see rows 3-5.*
3. **Catalog readers** (`persistant_relational::catalog`, over the `RelationalBackend` trait: PostgreSQL, MySQL/MariaDB, SQLite) and **`persistant-migrate snapshot` / `--check`**. *Built, 0.8.22: writes UniSQL's `Snapshot` (unisql-core 0.24.17) from a live catalog or a scratch apply. Built, 0.9.3: a database-free source, `snapshot --from-files --dialect D` (UniSQL's `schema_from_ddl` over the migration files; `source = ddl`). A CI step in a consumer's repo is theirs to add.*
   *Row 5 (the catalog `Checker` in `persistant-macros`, `sql!` reading the snapshot): built, 0.9.0, with `schema = none` as the stated opt-out; checks: unknown columns and `INSERT` arity (UniSQL's), a `NOT NULL` column without default left out, `ON CONFLICT` not a unique key. Operand classes and argument/row types are row 6: *operand classes built, 0.10.0 (`WHERE` of a one-table `SELECT`/`UPDATE`/`DELETE`, PostgreSQL source only); argument types (a bound `$n` against its column's class) built, 0.10.4 (`docs/COMPILE-TIME-SQL.md` 7b, opt-in `params = (..)`, on UniSQL 0.27.0); the `ToValue` tuple method `to_values` of the design is not built; background: `query_sql` takes `&[Value]` at run time, so a compile-time check needs a typed call form.*
4. **`FromRow` derive and `sql!(as T, ..)`** (typed rows over the shared value classes). *Built, 0.9.2: by position, checked against the schema snapshot at build time (columns, names, classes, nullability); expression columns typed by alias annotation; joins unknown; `query_as` on every backend.*
5. **C6 (optional): `PgTx` / `MysqlTx` / `SqliteTx` `connection()` accessors**, so `sqlx::query!` can run inside a PersistAnt transaction. Verified missing at `4febf59`; additive; built only if a consumer asks (board 230 option A uses the pool, which already works).

## SQLite and MySQL backends (through UniSQL)

`KvOnRelational` written once in PostgreSQL SQL and translated by UniSQL (`DESIGN-KINDS.md` §5).
Depends on UniSQL 0.24.4 or later from crates.io.

## Patent conditions (docs/PATENT-SWEEP.md second pass, 2026-10-25): do not build without re-reading the patent
| Do not build | Re-read first |
|---|---|
| Run-time translation of a **caller's** SQL, a pre-populated or pre-verified translation library, a custom driver, interception or an ML step. The `sql!` macro stays compile-time; the run-time translator in `persistant-kv` (lazy cache) is allowed only for this crate's own constant statements, and no provider may declare `rel.source_dialect` without re-reading these and CireSnave's call (corrected 2026-10-29: the earlier row said no run-time cache anywhere, which the code contradicts) | US20140280259A1 (arguable, medium), US12079210B2 (arguable, low), US9582500B2, US12204528B2, US12147422B2 |
| SQL grammar encoded as a type state machine | US9177017B2 |
| The `sql!` check as a synthetic-type provider replaced at compile time | US8813027B2 |
| Backend selection that returns a closest match (an unmet need stays an error) | US9100343B1 |
| Emulating an unsupported backend operation from stored per-backend metadata | US9454548B1 |
| A rolling snapshot-plus-commit-log durability scheme | US20130198139A1 |

## Spec-to-roadmap audit (PM [TASK] 2026-10-24, CireSnave's idea; read at `dabef6c`, 0.9.46)

Read: `README.md`, `ROADMAP.md`, `SECURITY.md`, `bench/README.md`, `matrix/README.md`, `scripts/machine-durability/README.md` and `docs/` (`COMPILE-TIME-SQL`, `CONSUMERS`, `DECODER-AUDIT`, `DESIGN-AUTH-STORAGE`, `DESIGN-DURABILITY-MACHINE`, `DESIGN-KINDS`, `DESIGN-SQLX-BRIDGE`, `FACADE-MIGRATION`, `REGISTRY-FOR-STORE-USERS`): 15 files. `DESIGN-KINDS.md` section 11 is a snapshot at 0.4.24, so most of its "not built" text is stale; the rows below are only what the code still lacks (a name absent from `crates/`, `matrix/`, `src/` at `dabef6c`: `kv.in_relational_transaction`, `persist.kind`, `rel.trigger`, `rel.advisory_lock`, `rel.skip_locked` have 0 hits, while `lock.lease` and `ops.pitr` are present, so the search works). **None of these is started by this PR.** State: *open* (nobody holds it), *held* (by whom), *done* (PR or version).

| # | Work | Source | State |
|---|---|---|---|
| 1 | Typed-error vocabulary beyond SQLSTATE classes (only `rel.error.unique` exists) | `DESIGN-KINDS` 11.3 | open |
| 2 | A test for `blob.max_value_size` (`Need::MaxValueSize` is refused only where a backend reports a limit; the leaf is untested, so off) | `DESIGN-KINDS` 2.2, 11.3 | built (0.16.0): `BlobBackend::max_value_size`, `Leaf::MaxValueSize`, tested on a capped `MemoryBlob` with two known-bad defects; `memory` and `fs` are off as missing (no ceiling advertised) |
| 3 | `blob.file_mode` (file permissions of the `fs` blob driver; SQLite is done, see File permissions) | `DESIGN-KINDS` 2.2 | built (0.15.0): `OpendalBlob::owner_only()`, `tests/file_mode.rs`, matrix rows under `file_mode=owner_only` |
| 4 | `blob.replace_atomic` crash test (**done**, `crates/persistant-opendal/src/crash.rs`, matrix row `blob.replace_atomic` on for `opendal-fs` with `atomic_write_dir=set`; read at `0d91fda`) and `blob.durable` (open) | `FACADE-MIGRATION` 3, `DESIGN-KINDS` 11.3 | open |
| 5 | `ops.pitr` test (restore to instant *t*, compare with a snapshot taken at *t*) | `DESIGN-KINDS` 2.2 | open, needs infrastructure the PM provides |
| 6 | `kv.in_relational_transaction`: `kv` operations inside a relational transaction | `DESIGN-KINDS` 4.4 | design note `docs/DESIGN-KV-IN-RELATIONAL-TX.md` (0.16.5). **Scheduled: when a consumer requests kv-in-tx** (PM 2026-11-01: B starting from A confirmed; the in-tx `batch` skips the batch lock; work order 1 is not built until a consumer asks; not declined) |
| 7 | Fault injection (`Masked`, `Faulty`) for every kind, not only blob | `DESIGN-KINDS` 5 | **done:** `persistant_kv::Faulty` (0.11.8) and `persistant_relational::Faulty` (0.11.10); blob keeps its older `Faulty`/`Masked` in the facade |
| 8 | One set of `tracing` span and field names (`persist.kind`, `persist.backend`, `persist.op`, `persist.capability`) | `DESIGN-KINDS` 5 | **done (0.11.6):** names in `persistant_core::telemetry`, `Traced` for kv (0.11.4), blob (0.11.5) and relational (0.11.6) |
| 9 | Secret handle for connection config beyond `ConnectionSecret` (env var, file or `with-secret` reference, redacted `Debug`) | `DESIGN-KINDS` 5, 10 | **Design note `docs/DESIGN-SECRET-HANDLE.md`; outcome A confirmed (PM 2026-11-01, 0.16.7):** `ConnectionSecret::Env` already is the `with-secret` path for a launched program, documented in `docs/REGISTRY-FOR-STORE-USERS.md`. **Scheduled: when a consumer asks:** C (a `SecretSource` resolver) and D (a kind-neutral `Secret` in `persistant-core`). B (shelling out to `with-secret` from the library) is rejected. `zeroize`, when taken, is an ordinary current third-party dependency: not now. A failed connect was tested for the password (0.16.7): no leak |
| 10 | Opt-in live test at open (rolled-back `TEMP` objects), per program through its `Needs` (D3, ruled 2026-10-14) | `DESIGN-KINDS` 2.3, 9 | open |
| 11 | The registry `Handle` serves the kv kind only; blob and relational ids are reported unmet | `DESIGN-KINDS` 3 | **design note** `docs/DESIGN-REGISTRY-KINDS.md` (0.11.12); PM ruled all four work orders, blob first (2026-10-28). **Order 1 built (0.11.13):** providers listed on `blob.*` / `rel.*`, open refuses with `KindNotServed`. **Order 2 built (0.11.14):** `AnyBlob`, `Handle::blob()`, reserved `kv`/`kvx` keys. **Order 3 built (0.11.15):** `AnyRel`/`AnyTx`, `Handle::rel()`. **Order 4 built (0.11.17):** blocking `blob()`/`rel()`, guide, `examples/registry_blob.rs`. Row 11 done except the `sqlite-full` relational matrix rows (separate finding) |
| 12 | Unrowed `rel.*` and `kv.*` seed leaves: `rel.trigger`, `rel.advisory_lock`, `rel.skip_locked`, `rel.json`, `rel.array`, `rel.bulk_copy`, `rel.session_setting`, `rel.catalog`, `rel.unique_index.expression` / `.partial`, `rel.dialect`, `kv.prefix_list.exactly_once`. Untested is off; each is a leaf plus a test when a consumer needs it | `DESIGN-KINDS` 2.2 | open, on demand |
| 13 | Survey the SQL standard feature-code coverage of SQLite and MySQL (only PostgreSQL's was read) | `DESIGN-KINDS` 2.2, 10 | open, low |
| 14 | A `tokio-postgres` driver crate | `DESIGN-KINDS` 9 (D4) | held: only if a consumer shows the need |
| 15 | Non-Rust (TypeScript) bindings | `DESIGN-KINDS` 6, non-goal 11 | held: later, not this phase |
| 16 | Consumers' hand-written locks, append and atomic-replace sites moved onto PersistAnt (OverMind consent, fuel x4, mlmf x2, coderipper tool cache, synapse x3) and the `blob.append` torn-line repair | `DESIGN-KINDS` 7 | held: each consumer's lane |
| 17 | A relational audit backend for auth-framework | `DESIGN-KINDS` 7 | held: auth-framework lane |
| 18 | A consumer-CI recipe: the same suite against the consumer's own production backend, beyond Synapse's `run_all` (stage 1, item 3) | `DESIGN-KINDS` 2.3 | open, after item 3 |
| 19 | Migrations, lock helpers and expiry helpers for **non-SQL** stores (OpenDAL has no TTL or locks; emulated, with the limits documented). `lock.process` is done (0.9.27); `kv.lease` and `kv.purge_expired` are on the kv backends, not the blob stores | `README` "Not yet" | open |
| 20 | `Store` reimplemented over `BlobBackend` (so `Needs` become matrix lookups), then deprecated. D7 option A is **done** (the facade re-exports the kind crates behind features); B and C are not ruled | `FACADE-MIGRATION` 4-6 | held: **a CireSnave call** (breaking for lightbulb and agentlife); not proposed |
| 21 | Stale text that contradicts the code: `README` ("Nothing in it is implemented yet", "relational support ... is not built"), `DESIGN-KINDS` 11.1-11.3 and the 2.2 `blob.create_exclusive_atomic` row ("not OpenDAL", done by the driver since 0.9.41), `FACADE-MIGRATION` 2 (`create_if_absent` gap), and this file's own "main is 0.7.8" / "0.7.9" | all of the above | done for `README` and `DESIGN-KINDS` (0.10.1); `FACADE-MIGRATION` 2 was already current; this file's two version snapshots are now dated "was ... then" |
| 22 | Easy gate (portfolio rule `CIRESNAVE-EXPECTATIONS.md` 6.24, CireSnave 2026-10-28: "Easy gets users"): the `README` opens with a ~10-line hello-world; the examples compile in CI (`cargo build --examples` for the registry and `Store` ones); before each first or breaking-80%-path release a fresh-eyes trial (an agent given only the `README` reaches the example's result) is required and its transcript goes to the PM at the gate | `PM` NOTICE easy-gate | **built (0.12.2):** the `README` opens with the 10-line hello (`examples/hello.rs`, run by CI, checked identical by `tests/readme_hello.rs`); the fresh-eyes trial before each first or breaking-80%-path release stays a gate step, transcripts in the PR |
| 23 | Deprecate `Endpoint::Url(String)` in favour of `Endpoint::Secret` / `url_env` | PM ruling 2026-11-01 | **With next Breaking:** not now. 0.16.4 already redacts its `Debug`, and `#[deprecated]` fails consumers that build with `-D warnings` (configurant does). Do it in the next planned Breaking release (the second number), with a migration line in the CHANGELOG |

Counts: 15 files read, 21 rows added, 1 needs a CireSnave call (row 20). Rows 14, 15, 16 and 17 are held by design, not by a person who is blocked.
