# Changelog

## 0.11.16

### Added
- `easyants-card.toml` (the EasyAnts hub card, read from this repository on every hub build) and `examples/hello.rs`, the 10-line program it shows: open `memory` through the registry, put and get one key. CI runs the example (`cargo run --example hello`), and `tests/easyants_card.rs` checks the card against the hub rules (sentence length, the example exists, at most 10 lines shown, the range is the whole program). Stacked on 0.11.15 (#167).

## 0.11.15

### Added
- Registry, work order 3 of `docs/DESIGN-REGISTRY-KINDS.md` (audit row 11): `Handle::rel() -> Option<&AnyRel>`, `Some` exactly when the needs named a `rel.*` id. `AnyRel` is the enum of the built drivers (`sqlite`, `postgres`, `mysql`/`mariadb`, each plain and through `Bounded` when a call bound is given) and implements `RelationalBackend`; `AnyTx` implements `RelationalTx`. The second gate adds the relational surface from what was opened (backend, version, `dialect`). Tests: the relational contract through `AnyRel` equals the driver's on `sqlite`; statements, a rolled-back and a committed transaction, the call bound carried through; the same on each server in reach (CI containers).
- `OpenError::KindNotServed` is no longer returned (every kind a provider is listed for now opens); the variant stays so a `match` written against 0.11.13 or 0.11.14 compiles.
- Known gap, unchanged: `sqlite-full` has no relational matrix rows, so a `rel.*` need is not met by it.

## 0.11.14

### Added
- Registry, work order 2 of `docs/DESIGN-REGISTRY-KINDS.md` (audit row 11): `Handle::blob() -> Option<&AnyBlob>`, `Some` exactly when the needs named a `blob.*` id. `AnyBlob` wraps the OpenDAL store of `memory` / `fs` and implements `BlobBackend` (all nine operations and `key_layout`); `fs` opens the same directory the key-value store sits on. A needs list naming `blob.*` ids is no longer refused at open; one naming `rel.*` still is (`KindNotServed`, order 3). The second gate now builds one surface per kind from what was opened.
- Reserved keys: `kv`, `kvx` and anything under `kv/` or `kvx/` are refused by `AnyBlob` with `BlobError::Backend` naming the prefix (the key-value store keeps its files there on `fs`); a listing leaves them out. Applied on `memory` too, so a program does not behave differently by the id in its configuration. PM-approved 2026-10-28.
- Tests: the blob contract (`conformance::certify`) through `AnyBlob` gives the same leaves as the driver on `memory` and `fs`; reserved keys refused on every operation with lookalike names (`kvs/a`, `kvx2`) accepted as a control.

## 0.11.13

### Added
- Registry, work order 1 of `docs/DESIGN-REGISTRY-KINDS.md` (audit row 11): providers are **listed** on `blob.*` and `rel.*` capabilities as well as `kv.*`. Each provider has one matrix surface per kind it serves (`memory` and `fs` the blob store under their key-value store, `sqlite`, `postgres`, `mysql` and `mariadb` their relational database), the first kind a need names is the primary surface, and the durability shown is the weakest of the kinds the need names. A kind a provider does not serve is "no row" and unmet, never a closest match.
- `OpenError::KindNotServed`: a need on `blob.*` or `rel.*` that passes the gate is refused at open, before any I/O, until `Handle::blob()` / `rel()` exist (work orders 2 and 3). Until then the listing promises what open does not deliver for those kinds, and says so.
- Known gap, pinned by a test: `sqlite-full` has no relational matrix rows (only `sqlite` does), so it is not listed for `rel.*` needs.

## 0.11.12

### Added
- `docs/DESIGN-REGISTRY-KINDS.md`: design note (no code) for serving the blob and relational kinds through the registry (audit row 11): providers list one surface per kind, `Handle::blob()` / `rel()`, a shared-namespace rule for `fs` (`kv/`, `kvx/` reserved), the second gate per kind, weakest-class durability, four work orders, and one finding (`sqlite-full` has no relational matrix rows). Three decisions asked of the PM.

## 0.11.11

### Changed
- Dependencies to their latest (portfolio rule): `unisql`, `unisql-core`, `unisql-embed` and `unisql-macro-core` 0.27.0 to 0.27.10 (minimum raised in each crate; includes 0.27.6, which refuses `date_trunc('week'|'quarter', <param>)` on MySQL/MariaDB: PersistAnt uses no `date_trunc`), and the lockfile's `asyncband`, `ctutils`, `rustix`, `xattr`. No API change here.

## 0.11.10

### Added
- `persistant_relational::Faulty` and `FaultyTx` (audit row 7, relational part; completes row 7): the relational twin of `persistant_kv::Faulty`. `fail_nth` fails the Nth `execute`, `query`, `begin`, `tx.execute`, `tx.query`, `tx.commit` or `tx.rollback` (or the Nth call of any) before it runs; `interrupt_nth` runs it and then returns the error, so an interrupted `tx.commit` commits while the caller is told it failed; `mask` answers `Unsupported`. Counts span the backend and its transactions. Tests use a counting fake and read back what actually ran or committed; the unwrapped fake is the control. It is a wrapper, not a fake database.

## 0.11.9

### Changed
- `docs/PATENT-SWEEP.md`: the first-pass table's `unread` verdicts replaced by the second pass's (every one of those patents' independent claims was read there; US9582500B2 and US20140280259A1 are `design-around` conditions); WO2008115343A1 read (a vehicle crash-test sled, clear). Only the patents under "Still unread" remain unread. Docs only, prompted by the PM [NOTICE] of 2026-10-26 on CodeRipper 0.10.0's `patent-sweep-record` rule.

## 0.11.8

### Added
- `persistant_kv::Faulty` (spec-to-roadmap audit row 7, kv part): wraps any `KvBackend` so a consumer can test its store misbehaving. `fail_nth` fails the Nth call (of one operation, or of any) before it runs; `interrupt_nth` runs the call and then returns the error, the ambiguous outcome of a lost reply; `mask` makes an operation answer `Unsupported`; `calls` counts. Rules fire once. Tests use `MemoryKv` and show the effect (or its absence) by reading back; the unwrapped backend is the control for the mask. The relational kind is not built (it needs the statement/commit boundary, `DESIGN-KINDS` section 5).

## 0.11.7

### Added
- `bench/opendal-speed-concurrent-64.tsv`: concurrent timings at value size 64 (`--tasks=1,4,16 --ops=50`), PM quiet window 2026-10-25 21:00-21:20Z (run 21:00:10-21:09:12Z), Windows, 32 threads, binary built from 0.11.6 at 20:52Z. 45 rows measured, 18 excluded (same split as the 256 and 4096 files); no row shows 50% or more CPU at either end of its `host=` reading. Data only, no code change.

## 0.11.6

### Added
- `persistant_relational::Traced` and `TracedTx` (feature `tracing`; facade feature `relational-tracing`): the relational counterpart of the kv and blob wrappers, same `persist.*` names with `persist.kind = "rel"`; spans for `execute`, `query`, `begin` and a transaction's statements, commit and rollback. The outcome is `ok` or the error kind; no SQL text, parameter, row or error text is a field. This completes audit row 8.

## 0.11.5

### Added
- `persistant_blob::Traced` (feature `tracing`; facade feature `blob-tracing`): the blob counterpart of the kv wrapper, same `persist.*` names with `persist.kind = "blob"`, outcome `ok`, `backend` or `unsupported`, no key, value or error text in any field. Relational wrapper not built yet (audit row 8).

## 0.11.4

### Added
- `persistant_core::telemetry`: the one set of `tracing` names (`persist` span; fields `persist.kind`, `persist.backend`, `persist.op`, `persist.capability`, `persist.outcome`) from `docs/DESIGN-KINDS.md` section 5 (spec-to-roadmap audit row 8).
- `persistant_kv::Traced` (feature `tracing`; facade feature `kv-tracing`): wraps any `KvBackend` and emits one `DEBUG` span per call. No key, value or error text is ever a field (the outcome is `ok` or the error kind). Tests capture the spans with a `tracing-subscriber` layer; a backend whose error quotes the key is the control that the leak check can fail. Blob and relational wrappers are not built yet.

## 0.11.3

### Added
- `docs/PATENT-SWEEP.md` second pass: full independent claims of 29 first-pass hits, 2-3 new queries per idea (Google Patents only; Espacenet and WIPO unreachable), no needs-review; `ROADMAP.md` gains six "do not build without re-reading" patent conditions.

## 0.11.2

### Added
- `docs/PATENT-SWEEP.md`: first-pass patent sweep of the 0.11.x ideas (PM process of 2026-10-25). Thin coverage; five hits with unread claims are listed for review. Not legal advice.

## 0.11.1

### Added
- `bench/opendal-speed-concurrent.tsv` (value size 256) and `bench/opendal-speed-concurrent-4096.tsv`: first concurrent timings (`--tasks=1,4,16 --ops=50`), PM quiet window 2026-10-25 13:09-13:35Z, Windows, 32 threads, run with the 0.11.0 binary. Rows whose `host=` shows 50% or more CPU at either end are not evidence (one in the 256 file, six in the 4096 file, including some 16-task rows whose end load may be their own). Value size 64 not yet run.

## 0.11.0

### Added
- `speed::run_concurrent(target: Arc<K>, .., ops_per_task, tasks, value_bytes, probe)`: the key-value workloads with N tasks at once (each its own keys, started together after setup). Its own workload in the data file (`speed.kv.put.c8`, with a size `speed.kv.put.v4096.c8`); `ops_per_sec` is total operations over wall-clock time. One task is `run_sized`. `Workload::id_with`.
- `bench::host_snapshot()` (`cpu_load_pct`/`load1` and `ram_available_mb`), recorded at the start and end of every concurrent workload in the row's environment (`host=`), so a figure taken on a busy machine says so.
- Example `speed_concurrent` (persistant-opendal). No data file is committed: the timing runs wait for a quiet machine (PM window).

### Changed
- **Breaking:** `bench::Environment` gains `host: Option<String>`; struct literals must add `host: None`. `Environment::detect` is unchanged. Data files without `host=` still parse.
- `persistant-kv` depends on `tokio` (`rt`, `sync`) for the concurrent harness.

## 0.10.5

### Added
- `speed::compare_across(table, &[workload ids])` (`Profile`, `Position`): each backend's place in several workloads side by side, per environment, plus the backends another one beats on every workload (the Pareto front is whoever nobody beats). No combined score: how to weigh workloads is the consumer's call. `speed_report --across=ID,ID` prints it.

## 0.10.4

Published to crates.io (tag `v0.10.4`). 0.10.0 to 0.10.3 were never published (the latest before this was 0.9.42), so their changes (0.10.0 operand classes, 0.10.1 and 0.10.2 docs, 0.10.3 value-size axis) first reach crates.io here.

### Added
- `sql!(.., params = (i64, String), "..")`: the Rust type of each `$n`, checked against the plain column it meets in `WHERE col = $n`, `INSERT INTO t (cols) VALUES ($1, ..)` and `UPDATE t SET col = $n`. Text against a number or boolean (and the reverse) is a compile error naming `$n`, the type and the column; a boolean against a number only for a PostgreSQL source. Unknown types (`Uuid`, dates, JSON, aliases), expressions and joins pass; nullability is not checked; the count must equal the statement's parameters. Opt-in: a call without `params` is unchanged. `docs/COMPILE-TIME-SQL.md` 7b.

### Changed
- UniSQL 0.26.0 -> 0.27.0 (`CheckContext.param_types`). `persistant-relational` re-exports UniSQL types in its public API (`catalog::{Snapshot, SnapshotError, Column, ColumnClass, Schema}` with the `catalog` feature, and `embed` = `unisql_embed`), so a consumer that also depends on `unisql` should move to 0.27.0 together with this release.

## 0.10.3

### Added
- `persistant_kv::speed::run_sized` and `Workload::id_at`: time the key-value workloads at any value size. The default 256 bytes keeps its ids; another size is its own workload (`speed.kv.put.v4096`), so `compare` never ranks different sizes together. `choose::need_sized` ranks by a sized workload. The code half of the ROADMAP "value sizes other than 256 bytes" gap; no data file is regenerated yet (needs a quiet machine).

## 0.10.2

### Changed
- Docs: design for argument types in `sql!` (`docs/COMPILE-TIME-SQL.md` section 7b): opt-in `params = (i64, String)`, class-only check, agreed with the UniSQL lane; the build waits for UniSQL 0.27.0 (`CheckContext.param_types`).

## 0.10.1

### Changed
- Docs: stale text corrected (PM audit row 21). `README.md` no longer says nothing of the wider layer is implemented or that relational support is not built; `docs/DESIGN-KINDS.md` section 11 is marked as a 0.4.24 snapshot with corrections, and 11.3 lists only what is still not built; `ROADMAP.md` dates its two old version snapshots.

## 0.10.0

### Breaking
- `sql!` calls that compare a plain column with an operand PostgreSQL has no operator for (text against a number or boolean, a number against a boolean) no longer build; see below.

### Added
- **`sql!` checks comparison operand classes** (ROADMAP compile-time SQL row 6, PM [TASK] 2026-10-24): in the `WHERE` of a `SELECT`, `UPDATE` or `DELETE` over one table, a plain column compared with a literal or another column of the same table is a compile error when PostgreSQL has no operator for the two classes (text against a number, a number against a boolean). Only for `source = postgres` (SQLite and MySQL convert silently); a string literal against a numeric column is never reported (PostgreSQL converts it); joins, subqueries, CTEs, expressions and unknown columns pass. Tests `comparison_operand_classes_*` give each fault with a passing twin and the cases that must not be reported.

## 0.9.48

### Changed
- Dependencies: UniSQL 0.25.8 -> 0.26.0 (`unisql`, `unisql-core`, `unisql-embed`, `unisql-macro-core`; CireSnave: all dependencies on their latest). UniSQL 0.25.8 rejects a literal `NULL` for a `NOT NULL` column with no default, and 0.26.0 (Breaking) needs `source_collation_not_bytewise` for `LIKE`/`ILIKE` on a non-bytewise collation; both can add compile errors to a consumer's `sql!` calls. No call site in this repo changed.

## 0.9.47

### Changed
- Docs: spec-to-roadmap audit (PM [TASK] 2026-10-24): `ROADMAP.md` gains a section with 21 rows for work that a design doc or the README names and the roadmap did not (15 files read, one needs a CireSnave call). Nothing started.

## 0.9.46

### Changed
- Docs: the compile-time SQL interface with UniSQL is frozen (`docs/COMPILE-TIME-SQL.md` section 7; PM [TASK] 2026-10-24, ROADMAP row 1): `Snapshot` serde and format, `Column` detail fields, `Sql`, schema lookup and `Checker::check` are stable within a UniSQL `0.n` line, with a `[NOTICE]` before a breaking change. Records the checks UniSQL 0.25.8 and 0.26.0 added, which can add compile errors to existing `sql!` calls when the dependency is raised.

## 0.9.45

### Changed
- CI: the portfolio workflow is pinned to coderipper 0.9.14 (d14a22b), which installs coderipper 0.9.14 (the old pin installed 0.9.6).

## 0.9.44

### Added
- **`Matrix::to_capant()` and `capant_provider(..)`: the rows as a `capant` matrix** (PM [TASK] 2026-10-24, capant step 2, PR-a; no behaviour change, no call site uses them). `to_capant` turns every row into a `capability` row for `capant` (the lookup ignores `subject_kind` too), a `yes` row's probe id becoming its evidence and `Reason::Untested` becoming `No(Other("untested"))` so that "a `no` row wins" stays exact; `capant_provider(id, backend, version, actual)` is the one place `Matrix::lookup` is asked about, as a `capant` provider.
  - Test `tests/capant_view.rs`: for every row of the five checked-in matrices against every configuration they name (more than a thousand questions), `capant::resolve` gives the verdict the gate gives (`Yes` / `No(reason)` / `Unverified(no row)`), and `capant::evaluate` says a provider satisfies a need exactly when the gate turns every capability in it on (each capability alone and each backend's whole set). A control flips a verdict in the view (yes to no, no to yes) and the same comparison reports it.
  - **One intended difference, tested:** a `yes` row with no probe id is `Unverified(no evidence)` to `capant` and on to the gate. No checked-in row lacks one. Step 2's later PRs decide whether the gate adopts `capant`'s stricter reading.
  - `capant` is a dev-dependency of the root crate for the test (it was already a dependency of `persistant-core`).

## 0.9.43

### Added
- **SQLite databases are owner-only on Windows too** (PM [TASK] 2026-10-24, ROADMAP "File permissions"). `SqliteBackend::open` and `open_durable` create a new database file with a protected ACL holding one entry, full control for the user the process runs as, instead of the directory's inherited ACL (usually readable by other local users). It is set through the Win32 security API in one small Windows-only module of `persistant-sqlx` (`src/owner_only.rs`, dependency `windows-sys`, only on Windows); nothing changes on other platforms. If the file cannot be restricted the open fails and the new file is removed.
  - `open_durable` also creates the `-wal` and `-shm` files restricted, because SQLite does not copy a Windows ACL to them as it copies a Unix mode. The rollback `-journal` of `open` is created by SQLite on every transaction and still inherits the directory's ACL; restrict the directory, or use `open_durable`.
  - A database file that already exists keeps its ACL, as on Unix. Files written by the `fs` blob driver are not restricted on either OS.
  - Test: `crates/persistant-sqlx/tests/sqlite_file_acl.rs` reads the ACL back with `icacls`, with a control (an ordinary file is not owner-only by the same reading).

## 0.9.42

### Changed
- **The registry's `fs` keeps every key apart on any filesystem** (PM [TASK] 2026-10-24, ROADMAP consumer row 9). A key was a file path (`kv/<key>`), so on a case-insensitive filesystem (NTFS, APFS by default) `Key` and `key` were one file, a key such as `a` could not sit beside `a/b`, and a reserved name (`CON`) or `..` was a path. `BlobBackend` has a new provided method `key_layout()` (`KeyLayout::Raw` by default); `OpendalBlob::fs` returns `KeyLayout::HexPath`, and `KvOnBlob` then stores each key's bytes in lower-case hex, in path segments of at most 100 characters, under `kvx/` and ending in a `-` segment. Bytewise order and the prefix relation are kept, so `ordered_scan` and `prefix_list` behave as before; a store with the raw layout (`memory`, any other blob store) is unchanged.
  - **Migration:** a directory written by 0.9.41 or earlier holds `kv/<key>` files. `registry::open_named("fs", ..)` moves them to the new layout when it opens the directory, and `KvOnBlob::migrate_raw_layout()` does it for a program using the adapter itself (an exclusive copy, then a delete; a newer value at the new key is never overwritten; expired records are dropped; repeating it is harmless). A raw `fs` directory read by an older version after this one has moved it will not see its keys.
  - `open_named("fs", ..)` no longer refuses a directory on a case-insensitive filesystem, and `OpenError::CaseInsensitiveFilesystem` is no longer returned (the variant stays so `match` arms compile). The probe that fed it is removed.
  - Tests: `crates/persistant-kv/src/on_blob.rs` (round trip, no collision even ignoring case, no key a directory of another, order and prefixes over awkward keys and the segment boundary, a case-folding store as control, migration), `crates/persistant-opendal/tests/kv_on_fs_keys.rs` and `tests/registry_fs_case.rs` (a real directory; passes on NTFS), and the kv conformance for `fs` no longer expects `kv.get` off on a case-insensitive filesystem.
  - `MemoryBlob::with_hex_layout` exists to test the layout without a filesystem.
- The advisory `portfolio` CI job is pinned to coderipper `30e012b7` (concurrent-run install fix; was `a48fc6cc`).

## 0.9.41

### Added
- **`fs` with `atomic_write_dir` now creates exclusively** (PM [TASK] 2026-10-24, ROADMAP consumer row 8). `OpendalBlob::create_exclusive` under a scratch directory writes the whole value to a new scratch file, fsyncs it, and `hard_link`s it to the key: the link is atomic and fails if the key exists, so of many concurrent creators exactly one wins, and the value is whole the moment the key exists. OpenDAL's own create there is check-then-rename (README 0.4.6, 0.4.7). So `blob.create_exclusive` and, over `KvOnBlob`, `kv.put_if_absent` are on for the registry's `fs` (`atomic_write_dir=set; put_mode=atomic`), which now meets `CreateIfAbsent` together with an atomic `kv.put`.
  - Needs the scratch directory on the root's filesystem and a filesystem with hard links; where the link is refused the answer is `Unsupported` (the leaf is recorded off), never a fallback to check-then-write. A key that is not a plain relative path (`..`, empty) is refused.
  - Tests: `crates/persistant-opendal/tests/fs_create_exclusive.rs` (16 contenders x 30 rounds on a multi-thread runtime, the value checked whole, no scratch file left; a control where check-then-write lets all 16 win), and the two `fs` conformance tests that said `create_exclusive` was missing now say it is on.
  - `matrix/opendal.tsv` is regenerated for this (the `opendal-fs` `atomic_write_dir=set` rows of `blob.create_exclusive` and `kv.put_if_absent`).
  - Unchanged: `Store::create_if_absent` (the old facade) still refuses `atomic_write_dir`.

## 0.9.40

### Changed
- **The matrix file is read and written by `capant` 0.0.7** (PM [TASK], step 1 of the capant move; no behaviour change). `Matrix::parse` and `Matrix::to_tsv` delegate to `capant::parse` / `capant::write` through one adapter (`persistant-core/src/adapter.rs`); `Row`, `Reason`, `Config`, `Lookup`, `CapabilityId` and `Matrix::lookup` are as they were, and nothing selects, opens or ranks differently.
  - **The file spells the probe id `probe=`** where it said `test=`. Rows still carry it as the `test` metadata key (every producer and every `r.metadata.get("test")` is unchanged), and a file with the older `test=` spelling is still read. The five `matrix/*.tsv` are regenerated: each of their 292 rows changes in that one key (and its place among the sorted metadata keys) and nowhere else.
  - **Proof of no change:** `tests/matrix_reader_equivalence.rs` keeps the 0.9.39 reader and checks that it and the new one read all five checked-in files to equal rows, that the gate's `lookup` is equal for every row against every configuration the files name, and that each file is exactly the writer's text.
  - **`ParseError`** gained `DuplicateColumn`, `BadTimestamp` and `Other` (it is `#[non_exhaustive]`). A header naming a column twice is now refused; `capant`'s `UNVERIFIED: ` reasons are still refused as `BadReason`, with the reason as written.
  - The optional `capant` columns (`probed_at`, `tool_version`, `environment`, `inherited_from`) are read into `Row::extra` under their own names, so a rewrite keeps them. The `persistant-core` crate has one dependency now: `capant`.

## 0.9.39

### Changed
- **The `LICENSE-MIT` copyright holder is now "Eric Evans"** (was "CireSnave"), for the repositories under `ciresnave/EvansLaboratories` (CireSnave's ruling on board item 263, relayed by the PM 2026-10-24: "Go with your recommendations"). The year is kept (2026) and no other holder text changed. The licence terms are unchanged (`MIT OR Apache-2.0`).

## 0.9.38

### Fixed
- **The CodeRipper findings of the member crates, resolved** (the pin `a48fc6cc` sweeps each workspace member; PM decision 2026-10-23: resolve, not only record).
  - `lint-suppression` (4): every `allow` now carries a `reason` (`sqlx_bridge.rs` x2, `registry.rs` x2).
  - `unused-return-values` (2): the `persistant-macros` test helpers `ok` and `ok_in` returned the expansion that 10 of 13 callers threw away. The helpers that return it are now `expansion` and `expansion_in` (their six users), and `ok` / `ok_in` assert and return nothing.
  - `reachability` (9): nothing was removed. All nine are documented, tested, public items of published crates that nothing inside their own crate calls, and no portfolio project calls them either (`git grep` of eight repositories at `origin/main`, 2026-10-23); removing a public item is a breaking change, which here means 0.10 and a rewrite of every matrix row's version column. They are declared as public API with a reason and an owner, in `.coderipper.toml` for the root package (`VERSION`) and in a `.coderipper.toml` inside each member (`persistant-core`: `is_under`, `rank`, `SEED`, `seed`; `persistant-kv`: `count_prefix_paged`, `is_eligible`, `precision`; `persistant-sqlx`: `call_bound_from_env`), because CodeRipper reads a member's declarations from the member's own directory (tested with 0.8.19: a root file is ignored for a member). The three member files are excluded from the published packages. A crate-level form is being built by the CodeRipper lane and would replace them.
  - The `/// The crate version` doc comment of `persistant::VERSION` sat above `pub mod registry;`; it is above `VERSION` now.

## 0.9.37

### Added
- **`.coderipper.toml`: two `ci-security` waivers, each with its reason and owner** (PM decision 2026-10-23; CodeRipper lane: by design). The `power-loss` jobs of `machine-durability.yml` and `synapse-power-cut.yml` run on `[self-hosted, Windows, X64]` because they need the physical power-loss rig, which the rule reads, with the GPU as its proxy, as hardware only we have; they cannot run on a hosted runner. Neither workflow has a `pull_request` trigger. A waiver with a reason is the designed mechanism, not a silencing: the findings are still printed as `waived`. Checked locally with `coderipper sweep --profile portfolio --no-config` (0.8.19): both findings change from `Medium` to `waived`, nothing else changes.
- **The portfolio job is re-pinned to coderipper `a48fc6cc`** (was `789b0122`; PM notice 2026-10-23): that commit sweeps workspace members, which the earlier pin probably did not (this repository is a real package with eight member crates). New findings, if any, become ROADMAP rows, one per rule.

## 0.9.36

### Added
- **`Cargo.lock` is committed, and CI builds with `--locked`** (CodeRipper `SUP-001`, PM decision 2026-10-23: the `persistant-migrate` binary is an application, and every other repository of the portfolio commits its lock). `/Cargo.lock` leaves `.gitignore`; every `cargo build|test|clippy|check|run` step of `ci.yml` and the build of `machine-durability.yml` and `synapse-power-cut.yml` (Synapse commits its own lock) takes `--locked`. "Newest versions at all times" is kept by updating the lock (`cargo update`, then the tests), not by leaving it out. **A version bump now also moves the nine workspace packages in `Cargo.lock`** (`cargo update --workspace`, or any unlocked `cargo check`), or `--locked` fails.


### Fixed
- **A race between two tests of `tests/registry_servers.rs`**, found by the first CI run with `--locked`: every round trip used the key `registry-test/<pid>`, so two tests of one binary, against the same PostgreSQL, could delete each other's key between a put and a get. Each call now takes its own key.
## 0.9.35

### Fixed
- **The five checked-in matrix tests no longer read outside their crate** (CodeRipper `SUP-004`, PM decision 2026-10-23). `persistant-opendal/tests/checked_in_matrix.rs` and `persistant-sqlx/tests/checked_in_matrix{,_sqlite,_mysql,_mariadb}.rs` did `include_str!("../../../matrix/*.tsv")`, which a published crate cannot do. They now live in the root crate's `tests/` (`checked_in_matrix_{opendal,postgres,sqlite,mysql,mariadb}.rs`), which already carries `matrix/` in its package (`registry.rs` reads it), so there is still **one** copy of each file. They run under `cargo test -p persistant --all-features`; the two MariaDB and MySQL CI steps no longer name them. No test changed otherwise.

## 0.9.34

### Added
- **The advisory CodeRipper portfolio check in CI** (`portfolio` job of `.github/workflows/ci.yml`; PM [TASK] 2026-10-23, CireSnave: "Roll out advisory-only, non-blocking CodeRipper checks everywhere."). It calls `ciresnave/coderipper`'s reusable `portfolio.yml` at a pinned commit (`789b0122`) with a read-only token and no secrets. It is not a required check and no rule is silenced; its first run's findings (six rules, 15 findings) are recorded as ROADMAP rows, one per failing rule, in the new "CodeRipper baseline" section; none is silenced or scheduled.

## 0.9.33

### Fixed
- **`open_named("fs")` no longer aliases keys that differ only by case** (found while answering a review of the registry guide; measured on Windows/NTFS: `put("Key")`, `put("key")`, `get("Key")` returned the second value). 
  `open_named("fs", ..)` now probes the target directory (a probe file with both cases in its name, looked up with the case flipped, then removed) and on a filesystem that ignores case returns the new `OpenError::CaseInsensitiveFilesystem { id, path }`, whose message names the problem and the ids to choose instead. No key is written (the directory itself may have been created, and is left). The check is on the directory, not the operating system: macOS can have case-sensitive volumes, Linux can have case-insensitive directories. The `fs` listing carries the caveat as a note (`Listing::notes`). Red first on Windows (NTFS): the test failed with `get("Key")` returning the other key's value, and the real lookup is asserted to refuse there. The probe's decision is also tested with stand-ins for both answers, and an unwritable directory is an error, not "case-sensitive". Not done: a key encoding that would make `fs` work on such filesystems (ROADMAP row 9).

## 0.9.32

### Added
- **`persistant::blocking::registry`** (feature `registry`; ROADMAP "Portfolio adoption", the blocking wrapper): the registry for synchronous callers. `providers` is re-exported; `open_named` / `open_named_with` return a `KvHandle` whose methods (`get`, `put`, `delete`, `put_if_absent`, `prefix_list`, `ordered_scan`, `ordered_scan_values`, `compare_and_swap`, `compare_and_delete`, `take`, `count`, `ttl_remaining`, `purge_expired`, `batch`) block instead of returning futures. It uses the worker of `blocking::Store` (one thread, a private single-threaded runtime), so it is safe from plain code, from many threads at once, and from inside another runtime; keep one handle for the life of the program. A backend that cannot give a method returns `Unsupported` as the async handle does, and a need the backend does not meet is `OpenError::Unmet` at open. Tests: a plain thread, `Unsupported` kept honest on `fs`, `Unmet`, 8 threads racing `put_if_absent` (memory) and `take` (SQLite), a call from inside a tokio runtime.

## 0.9.31

### Added
- **`docs/REGISTRY-FOR-STORE-USERS.md` and `examples/registry_instead_of_config.rs`** (ROADMAP "Portfolio adoption" ask 1): how a program that names its backend in code (`Store` + `Config::Fs`: commy, agentlife, lightbulb's disk tier) moves to `registry::providers` / `open_named`, with the mapping from each `Need` to a capability id measured against the matrices. What does not map is stated: `CreateIfAbsent` is off on the registry's `fs` (its scratch-directory configuration makes the exclusive create check-then-rename), `Stat` and `MaxValueSize` have no key-value counterpart, and the registry serves the key-value kind only, so append, locks and a blocking call stay outside it. Found by running the example: asking `fs` for `put_if_absent` lists no provider at all.

## 0.9.30

### Documented
- **Pool options on `PgBackend` are not a gap** (`docs/CONSUMERS.md`, `ROADMAP.md` ask 3): lightbulb builds its own pool because its `Cargo.toml` says `PgBackend::connect` offers no acquire timeout, but `PgBackend::connect_with(&ConnectionSecret, &ConnectOptions { tls, max_connections, acquire_timeout })` has existed since 0.4.31. Its default TLS policy is `Require`.
- **A roadmap row for the registry's `fs` having `kv.put_if_absent` off** (ask 8): found by running the registry guide's example; the configuration with a scratch directory makes the exclusive create check-then-rename.
- **A roadmap row for the registry's `fs` aliasing keys that differ only by case** on a case-insensitive filesystem (ask 9; measured on Windows through `open_named`, and documented in the registry guide).

## 0.9.28

### Documented
- **`ROADMAP.md`: a "Portfolio adoption" section** (docs only) carrying the seven asks of `docs/CONSUMERS.md` section 5 as rows with their state (the `lock.process` row is built in 0.9.27; the rest unscheduled), and the adoption order the survey ranked.

## 0.9.27

### Added
- **A matrix row for `lock.process`** (`std-file-lock`, `File::try_lock`): `persistant_opendal::certify_process_lock` starts a child of the host's own executable that holds the lock, checks the parent is refused, kills the child hard and checks the parent is then granted it; the controls are the same observation on a live child and the known-bad marker file, which survives the same kill. `maybe_run_lock_child` is the child's entry; the `matrix` example calls both, so `matrix/opendal.tsv` now records it (it was implemented and tested but in no row).

### Fixed
- **A wrong claim in 0.9.26 / `docs/CONSUMERS.md`**: they said nothing in the vocabulary names encryption at rest. It is `storage.no_plaintext_at_rest` (a canary value written through the layer is not found as raw bytes in the data directory, log or temp files), reserved with no implementation or row. The search had been for the word "encrypt". Corrected in both places.

## 0.9.26

### Added
- **`docs/CONSUMERS.md`** (docs only; PM [TASK] 2026-10-23, CireSnave's 2026-10-21 standing order): a survey of how twelve projects store things, read at their `origin/main` with the method, positive controls and limits stated. Per project: what it stores, the backend it names today (registry, `Store` + `Config`, or a backend directly), the needs it declares, the PersistAnt capability each maps to, and what blocks it; a ranked adoption list (auth-framework, commy, agentlife, lightbulb, synapse, Humboldt; five projects are not candidates) and what it asks of PersistAnt. Found on the way: agentlife's `"0.4.4"` pin cannot take 0.9.x; `lock.process` is implemented but has no matrix row; `blob` has no streamed or ranged I/O. (It first said nothing in the vocabulary names encryption at rest; that was wrong, see 0.9.27: the leaf is `storage.no_plaintext_at_rest`.)

## 0.9.25

### Documented
- **`ROADMAP.md` brought up to date** (docs only): the fresh-install check and the migration loader (C2, C3) are marked built; new sections for the sqlx bridge (work orders A to D built, E not scheduled, F already built), the key-value additions for auth-framework (`take`, `count`, `Indexed`) and file permissions (SQLite `0600` on Unix; Windows open, with the scope question at CireSnave).

## 0.9.24

### Added
- **`persistant_kv::index::Indexed` (`kv.index`)**: secondary-key lookup as a layer over `batch` and `ordered_scan_values` (`docs/DESIGN-AUTH-STORAGE.md` section 4.3; last of its three steps). `insert`, `replace` (if the record still holds the expected value) and `remove` move a record and its index entries in one batch; `lookup` scans the entries under a prefix from one snapshot and reads each record, `lookup_one` follows a single entry. A backend without `batch` or `ordered_scan_values` (a blob store) returns `Unsupported` and the leaf is `no`: an index that can diverge from its records is worse than none. New conformance leaf (insert/replace/remove, stale expectations, eight contenders replacing one value ten times, entries matching the winner) with known-bad controls `BatchIgnoresRequire` and `BatchNotAtomic`; a vocabulary entry; one new matrix row per kv backend.

## 0.9.23

### Added
- **`KvBackend::count` (`kv.count`)**: the number of live keys beginning with a literal prefix, from one consistent snapshot (`docs/DESIGN-AUTH-STORAGE.md` section 4.2). Native on `MemoryKv` and on the SQL engines (one `SELECT count(*)` over the same key range as `prefix_list`); the default, and a blob store, is `Unsupported`, so the matrix says `no` there. New conformance leaf (neighbouring prefixes, `%` `_` `[` as literal characters, a delete, an expiry) with two known-bad controls (`CountIncludesExpired`, `CountIgnoresPrefix`). `persistant_kv::count_prefix_paged` counts by paging `prefix_list` for a backend without it and is documented as not a snapshot; the matrix does not claim `kv.count` for it. One new matrix row per kv backend.

## 0.9.22

### Fixed
- **UniSQL 0.25.8** (was 0.25.1; `unisql`, `unisql-embed`, `unisql-core`, `unisql-macro-core`). Its SQLite schema reader gave `INTEGER PRIMARY KEY` (SQLite's rowid alias) no default, so `sql!` rejected a valid `INSERT` that left the key out (`column `id` of `t` is NOT NULL with no default`). New test `a_sqlite_rowid_alias_may_be_left_out_of_an_insert`, red on 0.25.1 and green on 0.25.8, with a control that a NOT NULL column without a default is still caught. 0.25.8 also rejects a literal `NULL` into a NOT NULL column in UniSQL's own checker (not duplicated here).

## 0.9.21

### Fixed
- **`SqliteBackend::open` and `open_durable` create the database owner-only on Unix** (`0600`; its `-wal` and `-shm` files follow the database file). They were SQLite's default `0644` less the umask, world-readable under the usual `022`: found by the 0.9.20 check of the per-tenant open path. A file that already exists keeps the mode its owner gave it. On Windows nothing changes: the file inherits the directory's ACL, and restricting it needs the Win32 security API or a call to `icacls`, neither of which this crate takes on; restrict the directory the databases live in.

## 0.9.20

### Added
- **`KvBackend::take` (`kv.take`)**: remove a key and return the live value it held, to exactly one of any number of concurrent callers (`docs/DESIGN-AUTH-STORAGE.md` section 4.1). The provided body is `get` then `compare_and_delete` of what was read, so a backend has `take` exactly when it has `compare_and_delete` (memory and the SQL engines: on; the OpenDAL blob stores: off, as for compare-and-swap); a backend may override it natively. New conformance leaf with two known-bad controls (`TakeNotExclusive`, `TakeLeavesValue`) on `MemoryKv`, a native `MemoryKv::take`, delegation in `Instrumented` and the registry's `AnyKv`. One new matrix row per kv backend.

### Documented
- **`SqliteBackend::open` on a path that is not there yet** (the per-tenant case; new test `sqlite_open_path`): it creates the file and not the directory; the file's mode is SQLite's own default, `0644` less the umask (measured on Linux: `-rw-r--r--` under umask 022, `-rw-------` under 077), so a database that holds secrets should be opened under `077` or created beforehand. The docs of `open` say so.

## 0.9.19

### Added
- **`docs/DESIGN-AUTH-STORAGE.md`** (docs only; PM [TASK] 2026-10-23): what auth-framework's `AuthStorage` needs from persistant-kv, per backend (native, emulable, impossible). Corrects the lane's finding: compare-and-swap, atomic batch and `ttl_remaining` already exist on memory and the four SQL engines (matrix at `ccad2b4`); only `take` and `count` are missing, and secondary-key lookup is a layer over `batch`, not a backend method. Recommends three small PRs (`kv.take` with a default over `compare_and_delete`, `kv.count`, an `Indexed` layer) and says what is unverified (per-tenant file creation and permissions, throughput).

## 0.9.18

### Added
- **`persistant_sqlx::OnPool` and `OnConn`** (feature `bridge`; work order D of `docs/DESIGN-SQLX-BRIDGE.md`): `Runner`s for an sqlx `PgPool` and `PgConnection`, so `sqlx_query!` statements run on the pool or inside the sqlx transaction a program already holds (`.fetch_all(OnConn(&mut *tx))`), and a module can move to the macros one statement at a time. The translated PostgreSQL text runs with sqlx's own binds and decodes like `PgBackend`; a `Uuid` or `timestamptz` placeholder is cast from sqlx's recorded type. Tested on PostgreSQL: pool, in-transaction (seen by a plain sqlx query in the same transaction, undone by its rollback). `persistant_relational::prepare` is now public for driver crates.

## 0.9.17

### Added
- **`sqlx_query!`, `sqlx_query_as!` and `sqlx_query_scalar!`** (feature `sqlx-query` of `persistant`; work order C of `docs/DESIGN-SQLX-BRIDGE.md`). They take sqlx's arguments, read the result types from sqlx's offline data (`.sqlx/query-<sha256>.json`: a missing or stale file is a build error that says `cargo sqlx prepare`), translate the literal through UniSQL for `targets = [..]` (default PostgreSQL), cast each `Uuid`, `chrono` or `json` placeholder from sqlx's recorded parameter types, and return a `Bound` statement: `.fetch_one(&db)`, `.fetch_optional`, `.fetch_all`, `.execute`. `sqlx_query!` generates the record struct, `sqlx_query_as!` checks the caller's `FromRow` struct against the described columns (names, count, nullability) at build time. New in `persistant-relational` (feature `sql`): `Bound`, the `Runner` trait (implemented for `&backend`) and `InTx` for a transaction. With feature `sqlx-query-checked` every literal is also checked by sqlx's own macro in a closure that is never called, and when our side fails sqlx's error is emitted beside ours. Checked on SQLite and PostgreSQL (`crates/persistant-sqlx/tests/sqlx_query.rs`, with hand-made `.sqlx` files), and by hand against a live PostgreSQL with sqlx's check on (wrong column, wrong argument type, renamed field). Not yet: running on a sqlx pool or transaction (work order D).

## 0.9.16

### Changed
- **UniSQL 0.25.1** (`unisql`, `unisql-embed`, `unisql-core`, `unisql-macro-core`; was 0.24.17). It translates `LEFT JOIN LATERAL (subquery)` and the `::regclass` cast, the two statements Humboldt's `src/` had refused.

## 0.9.15

### Added
- **`persistant-macros`: a reader for sqlx's offline query data** (`.sqlx/query-<sha256>.json`; internal, work order B of `docs/DESIGN-SQLX-BRIDGE.md`, no macro uses it yet). It finds a statement's file by the same hash sqlx uses, reads the column names, PostgreSQL types, nullability and parameter types, maps them to Rust types the way sqlx's PostgreSQL macros do (`chrono`, `uuid`, `json` on), honours the `"col!"`, `"col?"` and `"col: Type"` markers, and renders a record struct. A missing or foreign file is an error that says to run `cargo sqlx prepare`; a type without a default says what override to write. Checked against all 566 files of Humboldt's `.sqlx` (every file parses and every file name is the hash of its text; the test runs when `PERSISTANT_SQLX_SAMPLE_DIR` names such a directory).

## 0.9.14

### Added
- **`FromValue` for `uuid::Uuid`, `chrono::{DateTime<Utc>, NaiveDateTime, NaiveDate, NaiveTime}` and `serde_json::Value`, and a new `ToValue` trait for statement parameters** (features `uuid`, `chrono`, `json` of `persistant-relational`; `sql-uuid`, `sql-chrono`, `sql-json` on the facade). They read the canonical text the drivers already return (`docs/DECODER-AUDIT.md`) and send it back as text, so a statement that compares such a column casts the placeholder. A query_as struct with `Uuid` or `DateTime<Utc>` fields can now `#[derive(FromRow)]` (Humboldt: 24 of 24 DTOs were blocked on these). PostgreSQL arrays and decimals are still not read.

## 0.9.13

### Added
- **`docs/DESIGN-SQLX-BRIDGE.md`** (docs only, board 230 option 3): a macro family that keeps sqlx's own PostgreSQL check and runs the same literal through UniSQL and PersistAnt, typed from sqlx's offline `.sqlx` data. With the Humboldt measurement behind it: 883 literals, 881 translate, 2 refused, and the catalog checker covers `INSERT` and `UPDATE` only.

## 0.9.12

### Fixed
- **Concurrent first opens of a PostgreSQL database with no tables: the 0.9.11 retry was not enough.** On a real PostgreSQL 16 the 0.9.11 test failed with `type "persistant_kv_batch" already exists`, a second form of the same catalog race that the retry on a unique violation does not see (CI had passed it by timing). `KvOnRelational::open` now creates its two tables in one transaction behind `pg_advisory_xact_lock`, so concurrent first opens take turns; the retry is gone. Other dialects are unchanged. Test `concurrent_first_opens_all_succeed` (4 openers, 5 rounds, from empty) passed 5 of 5 runs locally against PostgreSQL 16 with the lock.

## 0.9.11

### Fixed
- **Concurrent first opens of a PostgreSQL database with no tables no longer fail one opener** (found by Synapse, 4 threads on a fresh PG16). `CREATE TABLE IF NOT EXISTS` is not safe against an identical concurrent statement in PostgreSQL: the loser got `unique violation (pg_type_typname_nsp_index)`. `KvOnRelational::open` now runs each table statement again (up to 5 times) on a unique violation, which finds the table the winner made. Test `concurrent_first_opens_all_succeed`.

## 0.9.10

### Added
- **`kv.bounded_call` is listed and opened as "bounded on open"** (for Synapse). The matrix learns a readable config fact `call_bound=set|unset`: the conformance row for `kv.bounded_call` now carries it (an unbounded backend's NO row is `call_bound=unset` and no longer vetoes a bounded open; the same backend through `persistant_sqlx::Bounded` has its own `call_bound=set` YES row, recorded by the matrix examples for sqlite, sqlite-full, postgres, mysql and mariadb). The other key-value leaves are not re-certified bounded: `Bounded` changes nothing below its bound. `registry::providers` evaluates a `kv.bounded_call` need with `call_bound=set`, so the database backends list it (text: "holds only when opened with a call bound") while `memory` and `fs` stay unmet. `registry::open_named_with(id, endpoint, needs, &OpenOptions::new().call_bound(d))` opens through `Bounded` and the second gate reads the bound back from the backend; `open_named` is `open_named_with` with default options. A bounded need without a bound is `OpenError::BoundRequired` before any I/O; a bound for `memory`/`fs` is `OpenError::BoundUnsupported`. `AnyKv` gains `SqliteBounded`, `PostgresBounded` and `MysqlBounded`.
- **`Handle::into_kv(self) -> AnyKv`**: the store owned, for a consumer generic over `K: KvBackend`.

## 0.9.9

### Fixed
- **The `kv.ttl` leaf no longer turns a correct backend off on a stalled machine.** It wrote an entry with a 1 s window and failed with "not readable inside its window" whenever more than a second passed between the put and the read (hosted Windows CI, `kv_on_sqlite_certifies_every_leaf_on`). The read is now judged only if the window was still open on the monotonic clock (measured from before the put to after the get); if it had already passed, the stall is printed ("kv.ttl: the 1000 ms window had already passed ... Repeating with a 4000 ms window") and the check repeats with a window four times longer, up to 16 s, then reports that the machine stalled. A backend that misses while the window is open still fails at once (test `a_stall_past_the_ttl_window_is_not_a_verdict_but_a_miss_inside_it_is`, with a backend that never keeps a time-to-live entry as the known bad); one that never expires entries still fails the later check (`Defect::TtlIgnored`). Expiry stays judged by the server's clock; no clock is injected.

## 0.9.8

### Added
- **`persistant::registry` serves the servers** (registry PR 4 of 4): features `registry-postgres` and `registry-mysql` (and `all-backends`, now every backend) add the ids `postgres`, `mysql` and `mariadb`. A server opens from a connection URL; `Endpoint::Secret(ConnectionSecret)` and `Endpoint::url_env(NAME)` keep it out of logs and `Debug` output. `open_named` refuses a server that is not the one the id names (`OpenError::ServerMismatch`: a MariaDB server behind `mysql`, or the reverse), and a server version the recorded matrix does not cover (the unmet ids). `Listing` gains `recorded` (the version the rows were recorded under) and its `why()` says "recorded on version X only". The MySQL list text carries the disk-full STALL caveat and the MariaDB text says no durability class is declared. New CI steps run `tests/registry_servers.rs` against the PostgreSQL, MySQL and MariaDB service containers.
- `OpenError::ServerMismatch` and `Endpoint::Secret` are additive on `#[non_exhaustive]` enums.

## 0.9.7

### Added
- **`persistant::registry` (features `registry`, `registry-sqlite`, `all-backends`): needs in, sorted backends out, the id kept in config opens the backend** (registry PR 3 of 4; CireSnave's design, `docs/DESIGN-KINDS.md` top). `registry::providers(&ProviderNeeds)` returns every backend compiled into the build that has all the needed capabilities, sorted by the preferred ones in order of importance, each `Listing` carrying a stable `id`, its endpoint kind, durability with basis, and `why()` text (preferred met and lacking, durability, caveats; `sqlite` vs `sqlite-full` explained). `registry::open_named(id, &Endpoint, &needs)` opens the backend the program stored in its config, checks the gate before touching anything and again for the backend actually opened, and returns a `Handle` whose `kv()` is the closed enum `AnyKv` (implements `KvBackend`, forwards every method). Ids today: `memory`, `fs`, `sqlite`, `sqlite-full`; they are append-only and never change meaning. A new backend module reaches a program when it upgrades `persistant` and rebuilds; there are no dynamic plugins. The handle serves key-value only, so a need for any other kind is reported unmet.
- `matrix/sqlx-sqlite.tsv` now also records the key-value leaves of `sqlite-full` (`SqliteBackend::open_durable`), so the registry can offer it on recorded facts.

## 0.9.6

### Added
- **`persistant_core::provider`: an order of importance, and the one call a setup UI makes** (registry PR 2 of 4). `ProviderNeeds::prefer(ids)` takes the ids that matter, most important first; a preferred id never excludes a provider. `sorted` orders qualified providers by the preferred ids in that order (having the first outranks any combination of the rest), then measured median time (unmeasured last, still listed), then name, and returns the evidence it sorted on (`Sorted`). `select` is `providers` then `sorted`; an unmet required set is still `Unmet`, whatever is preferred.

## 0.9.5

### Added
- **`persistant_core::provider`: "which backends can give me all of this?"** (registry PR 1 of 3; no driver, no I/O). `providers(&[Provider], &ProviderNeeds, &Matrix)` returns every provider whose recorded rows say `On` for every needed capability id (on at least one of its `Surface`s, `Off` on none) and whose declared durability reaches the minimum. Capability stays boolean. If nothing qualifies the result is `Unmet`: for every provider, each needed id it lacks with the matrix's own word (`Off(reason)` or no row) and any durability shortfall; there is no closest match and no partial list. `rank` orders the qualified set through the existing `choose()`. A `Provider` carries the `EndpointSpec` (none, path, or URL schemes) the registry's `open` will need; `open` and the closed `AnyKv` enum arrive in the facade (PRs 2 and 3).

## 0.9.4

### Fixed
- **`persistant-relational` `declared_durability` now says what the power-cut run showed.** It still read "no power-loss test" for every backend, though run 39178436801 (a Hyper-V guest powered off after 300 acknowledged writes, five cycles each, nothing lost; the `synchronous=OFF` control lost every key) had verified `Machine` through `KvOnRelational`, and the kv table already said so. `sqlite-full` (what `SqliteBackend::open_durable` reports, previously unknown here) is `Machine`, verified; `postgres` and `mysql` are `Machine`, verified; plain `sqlite` stays `Process` with nothing verified. Every basis is scoped to acknowledged key-value writes on ext4/Hyper-V and lists what was not exercised (multi-statement transactions, DDL, host power loss, NTFS, disk cache, torn sectors). The MySQL basis records that a full disk STALLS (no answer, no error; run 39325665814), so a caller needs a call bound (`call_bound`, `kv.bounded_call`).

## 0.9.3

### Added
- **`persistant-migrate snapshot --from-files --dir DIR --dialect postgres|mysql|mariadb|sqlite`**: the schema snapshot read from the migration files alone, with no database, no URL and no remote guard (ROADMAP compile-time SQL row 3, the database-free source). UniSQL's `schema_from_ddl` reads the migrations in version order (`CREATE TABLE`, `ALTER TABLE .. ADD|DROP|ALTER COLUMN`, `DROP TABLE`); statements it cannot read are listed on stderr and can only leave a column unregistered. `meta` records `source = ddl`, the dialect and each migration's version and checksum, so `--from-files --check` fails with exit 8 and names a migration added or changed since (`stale: migration 3 is not in the snapshot`). It is not the catalog's answer: types keep the migration's spelling and nothing the DDL does not state (a key, a default) is assumed; a catalog snapshot (`--scratch`) remains the exact one. `--from-files` with `--url`, `--scratch`, a missing `--dialect`, or on another command is a usage error (exit 2).
- Library: `persistant_migrate::snapshot_from_files(&[Migration], dialect)`.
- Tests: `persistant-migrate/tests/cli_from_files.rs` runs the binary: write, check current, stale after a new migration, and the refusals.

## 0.9.2

### Added
- **Compile-time SQL, stage 6: typed rows** (`docs/COMPILE-TIME-SQL.md` section 5; PM [TASK] 2026-10-20). `#[derive(FromRow)]` (macro in `persistant-macros`, trait in `persistant-relational`, both re-exported by `persistant` with feature `sql-macros`) reads a struct with named fields from a result row **by position**; the field types are the intersection the engines share: `i64`, `i32`, `f64`, `bool` (a boolean, or an integer 0 or 1), `String`, `Vec<u8>`, and `Option` of any (`FromValue`; an unsupported type is the compiler's "trait bound not satisfied", at the field). `sql!(as T, source = .., schema = .., "SELECT ..")` yields a `TypedSql<T>` and **checks the SELECT list against `T` at build time** with `const` assertions that name the column: the number of columns; each column's name (its alias, if any) against the field's; its class against the field's type (numeric into `i64`/`i32`/`f64`, text into `String`, boolean into `bool`, other into `Vec<u8>`/`String`; a class the schema does not state allows anything); and a column that can be NULL needs an `Option` field. An expression column is named and typed by its alias: `count(*) AS "n: i64"` (or `Option<i64>`). `SELECT *`, `UNION`/`INTERSECT`/`EXCEPT`, an untyped expression, a column the table (whose column list is complete) lacks, and `as T` with `schema = none` are build errors; with joins or subqueries the columns are unknown, which is "no check", never a wrong one. `RelationalRowsExt::query_as` / `RelationalTxRowsExt::query_as` run a `TypedSql<T>` and accept only the matching `T`.
- A read that fails says which column and why (`column 2 (field `email`): expected text, got Int(1)`); NULL into a field that is not an `Option` is an error, not a default.
- Tests: 30 macro unit tests (derive output and its refusals, the SELECT list read against the schema, each assertion's message, `as T` errors with passing twins), 3 on the row types, doc-tests with three `compile_fail` cases (wrong type, nullable into a non-`Option`, wrong name) and their passing twin, and `persistant-sqlx/tests/sql_macro.rs` running `ITEMS` and an annotated `count(*)` through `query_as` (and in a transaction) on real SQLite, PostgreSQL 16 and MySQL 8.4 against the committed snapshot. The build error for a wrong field type reads: `column 2 (`email`) is text: the type of field 2 cannot hold it`.

### Changed
- The default path the `sql!` expansion names `Query` through is now `::persistant::relational::embed` (was `::persistant::embed`, which still exists); `crate = path`, when given, is text-compared without spaces. With `as T`, `crate = ..` must end in `::embed`, so the checks are found beside it.

### Not done
- Joins and subqueries are not typed (their columns are unknown), `SELECT *` is refused, and finer types (UUID, decimal, timestamps) are read with the driver's own types (`docs/DECODER-AUDIT.md`); a record inferred from a live server for the statement as executed, as sqlx's `query!` does, is out of reach for a layer that runs one statement on several engines (design section 5).

## 0.9.1

### Fixed
- Machine durability harness, SQLite disk-full step: a 1 MB ballast file is left on the 8 MB volume and deleted before the check, as the PostgreSQL and MySQL volumes already do. Found by the first run of `synapse-power-cut` (run 39365280240): Synapse's `Mailbox::open` writes a batch, a volume with no free block refuses it ("database or disk is full"), so the 131 acknowledged messages could not be read back (UNVERIFIED, not lost). The pass criteria are unchanged: the sqlite-off control must still LOST, a SQLite/mailbox put must FAIL LOUD on the full volume, and every acknowledged key or message must be read back whole.

## 0.9.0

### Breaking
- **`persistant::sql!` now needs a schema decision** (`docs/COMPILE-TIME-SQL.md` section 3.2). A call states `schema = "path"` (relative to the crate's `Cargo.toml`; a `.json` snapshot from `persistant-migrate snapshot`, or a `.sql` DDL file) or `schema = none` (database-free checks only, said in words). With neither, the crate's `persistant-schema.json` is used, and if it does not exist **the build fails** naming the file and both ways out: a missing schema is never a quiet downgrade. Calls written against 0.8.21 and 0.8.22 add `schema = none` to keep their meaning. Reason: with no schema a comparison on an unknown column is taken for text and needs `allow = [text_collation]`; a stated schema removes that and enables the checks below.

### Added
- **Compile-time SQL, stage 5: the catalog checks** (PM [TASK] 2026-10-20, on UniSQL 0.24.17's `Checker` seam). With a schema, `sql!` runs UniSQL's name checks (an unknown column of a table whose column list is complete, `INSERT` row width, a column listed twice, `UPDATE .. SET` of an unknown column) and PersistAnt's own, both as build errors: an `INSERT` that leaves out a `NOT NULL` column with no default; an `ON CONFLICT (cols)` that is not, as a set, one of the table's stated unique keys. Every check errors **only when certain**: a table or column the snapshot does not state, a column list not marked complete, a key list marked unreliable (a partial or expression unique index exists), a flag the reader could not tell: all pass. Not checked, said plainly: operand classes in comparisons, the types of bound arguments and selected columns (stage 6), views, CTEs, other schemas.
- The schema file is tracked: the expansion `include_bytes!`s it, so cargo rebuilds when it changes. The one environment read is `CARGO_MANIFEST_DIR`.
- Tests: 23 macro unit tests (each check has a failing statement and its passing twin; a key list marked unreliable is not reported; a newer snapshot is refused; every `schema =` mistake is named), doc-tests with a `compile_fail` and its twin against `tests/doctest-schema.sql`, and `persistant-sqlx/tests/sql_macro.rs` now checking its statements against a committed snapshot (`sql_macro_schema.json`, produced by `persistant-migrate snapshot --scratch`) on real SQLite, PostgreSQL 16 and MySQL 8.4, with a test that the file is what the real SQLite catalog says. `FIND`'s `allow = [text_collation]` is gone: the snapshot says `id` is a number.

## 0.8.23

### Added
- `.github/workflows/synapse-power-cut.yml` (PM [TASK] 2026-10-20): Synapse's own power-loss test for its mailbox, `workflow_dispatch` only. Input `synapse_sha`: a full commit of `ThinkersJournal/synapse`; the workflow runs only from this repository's main branch, refuses a commit that is not an ancestor of Synapse's `origin/main` (proved with `git merge-base --is-ancestor`, not assumed), builds `mailbox_vm` (`synapse-mailbox-persistant`'s example) on a hosted `ubuntu-24.04` runner, and runs `scripts/machine-durability/run.ps1 -Binary <it> -Backends sqlite` on the self-hosted runner (conditions of 5.1g/5.1h: no pull_request trigger, no secrets). `run.ps1` is unchanged. **`-Backends sqlite`, not `sqlite,sqlite-off`** as `mailbox_vm.rs`'s header says: run.ps1 adds the known-bad control (`sqlite-off`, 4 cycles) by itself, and naming it in `-Backends` would make the control cycles "must hold" and send it to the PostgreSQL/MySQL disk-full volume.

## 0.8.22

### Added
- **Compile-time SQL, stage 4 part 1: the schema snapshot** (`docs/COMPILE-TIME-SQL.md` sections 1 and 4; PM [TASK] 2026-10-20). Feature `catalog` of `persistant-relational`: `catalog::read_catalog(&db, meta)` reads the catalog of the connection's current schema (PostgreSQL), database (MySQL, MariaDB) or file (SQLite) through the `RelationalBackend` trait alone and returns **UniSQL's `Snapshot`** (unisql-core 0.24.17: the file format is UniSQL's, this repo writes it). Each column carries its engine-spelled lower-cased `declared_type`, a comparison class from the engine's own rules (`catalog::classify`: `Text`, `Numeric`, `Boolean`, `Other`, or `Unknown` when no rule recognises the type, never a guess; MySQL's `tinyint(1)` is Numeric), `nullable` and `has_default`; each table's column list is marked complete and its unique keys stated (primary key first). Views, the migration ledger `_sqlx_migrations` and prefix keys are not in it. A table with a **partial or expression unique index has its key list marked unreliable** (so UniSQL derives no conflict key from it) instead of that index being silently dropped. `meta` records the dialect and, when derived from migrations, `migration.<version>` -> checksum. `catalog::parse` refuses a newer `format`; `catalog::differences(old, new)` names every change, including "stale: migration 37 is not in the snapshot".
- `persistant-migrate snapshot [--scratch --dir DIR] [--url URL] [--out FILE] [--check]`. Without `--scratch` it reads the target's own catalog; with `--scratch` it applies the directory's migrations to a scratch schema (PostgreSQL), scratch database (MySQL, MariaDB) or temporary file (SQLite), reads that and drops it, so the target is never touched (`PgBackend::fresh_catalog`, `MysqlBackend::fresh_catalog`). `--check` writes nothing and exits **8** if the file is missing, unreadable, newer, stale or different. `--allow-remote` guards it like every verb.
- Tests: the readers on real SQLite, PostgreSQL 16 and MySQL 8.4 (same tables, same structure, classes, keys and the unreliable flag back; what each engine must not report: view, ledger; a later change is seen), unit tests of the file (round trip, determinism, too-new refused, each kind of change named, the type classifier per engine), and the CLI end to end on SQLite, PostgreSQL and MySQL: a scratch snapshot leaves the target and the server clean, a changed migration makes the file stale (exit 8) naming the migration, the target's own snapshot after `up` equals the scratch one, a missing or newer file fails the check.

### Changed
- `unisql-macro-core` / `unisql-embed` / `unisql` are pinned at 0.24.17. 0.24.14's `Input` had no `schema` field; 0.24.17's does, and the unpinned minimum let CI resolve the newer one before the code was ready (PR #96's first run). `persistant::sql!` passes `schema: None` for now and reports `Kind::Catalog` problems at the statement.

### Not done
- Database-free snapshot from the migration files (source 1: `unisql::schema_from_ddl`) and `persistant::sql!` reading the snapshot with a catalog `Checker` (stage 5). MariaDB is exercised by the CI MariaDB job, not locally.

## 0.8.21

### Added
- **Compile-time SQL, stage 2** (`docs/COMPILE-TIME-SQL.md` section 6, step 2; PM [TASK] 2026-10-20 after UniSQL published `unisql-macro-core` and `unisql-embed` 0.24.13). New crate `persistant-macros` (a proc-macro, a thin wrapper over `unisql-macro-core`, since a proc-macro crate cannot be extended) and, behind the `persistant` feature `sql-macros`, `persistant::sql!`: it parses the statement in its `source` dialect, translates it for every named `targets` entry and embeds each target's text as a constant, so a syntax error, a skipped placeholder (`$1, $3`), an untranslatable construct or a missing allowance is a **build error** naming the target and the reason. Arguments: `source`, `targets` (both required: no hidden defaults), `allow`, `requires`, `forbids`, `crate = path` (default `::persistant::embed`); the statement is the last argument. Errors point at the argument that is wrong; several are all reported; the macro returns `compile_error!` tokens, never panics.
- `persistant::Sql` (UniSQL's `Query`), `persistant::embed` (UniSQL's `unisql-embed`, so a consumer depends on neither UniSQL crate) and `RelationalSqlExt::{execute_sql, query_sql}` on every `RelationalBackend` (feature `sql` of `persistant-relational`): the backend's own `Dialect` picks the embedded text and the target's parameter order is applied (a target may repeat or reorder placeholders). `RelationalTxSqlExt` does the same inside a transaction, with the dialect passed in (a transaction does not know it). A dialect the call did not name is `RelError::Unsupported`, a wrong parameter count is an error before anything runs, and an array-bound parameter is refused until `Value` carries one. The core traits are unchanged.
- Tests: 16 unit tests of the macro on `proc_macro2` tokens (one failing call per check, each with a passing twin; the refusal messages are asserted, not just "an error"), 5 of the extension traits on a fake backend, doc-tests including a `compile_fail` with its passing twin, and `persistant-sqlx/tests/sql_macro.rs` running the same checked statements, with a placeholder used twice and in a transaction, on real SQLite, PostgreSQL 16 and MySQL 8.4 (the CI MariaDB and MySQL jobs run it too).
- **Not done, said plainly:** no schema is read yet, so no table or column check and no knowledge that a column is numeric (comparing or ordering one needs `allow = [text_collation]` named at the call, as the end-to-end test shows); that is stages 3-5. Typed rows (`FromRow`) are stage 6.

## 0.8.20

### Added
- **`kv.bounded_call`** and an optional per-call timeout for the relational backends (PM [TASK] 2026-10-19; the MySQL full-disk stall of run 39325665814 is the real-world case). `persistant_sqlx::Bounded::new(backend, bound)` wraps any `RelationalBackend`: a call that takes longer than `bound` returns the new `RelError::Timeout { after }` (`KvError::TimedOut` through `KvOnRelational`) instead of hanging. The default is no bound; `call_bound_from_env(var)` / `parse_bound(text)` read one in milliseconds from a variable the caller names. A transaction that saw a timeout is poisoned (every later call and `commit` return an error) and rolls back on drop, so a batch that timed out applies nothing; a single statement outside a transaction, or a timed-out commit, has an unknown outcome (documented on `RelError::Timeout`).
- `RelationalBackend::call_bound` and `KvBackend::call_bound` report the bound (`None` by default). The leaf `kv.bounded_call` is declared **only** when one is configured: for an unbounded backend it is off as `missing` ("no call timeout configured"), recorded that way in the checked-in matrices. For a bounded `KvOnRelational` the probe (`KvBackend::bounded_call_probe`, over the new `RelationalBackend::with_lock_held`) holds the batch row's lock in a second transaction, runs a batch against it, and requires: the error is the timeout, it arrives within 3 x bound + 2 s, the key was not applied once the lock was released, and a normal batch works afterwards. Controls: `MemoryKv::bounded()` (correct) passes; `Defect::BoundIgnored` and `Defect::TimeoutLeavesBatchApplied` fail. Proven on SQLite, PostgreSQL 16 and MySQL 8.4.
- `OpCount::timed_out`, `KvError::TimedOut`.
- Tests (`persistant-sqlx/tests/bounded.rs`): a fake backend that waits forever on a statement (the timeout, the unbounded control that really hangs, a poisoned transaction that never commits), a real SQLite writer lock, the leaf on and off. Mutation: replacing the `timeout` in `Bounded` with a plain `await` fails 4 of 6 of them ("the call hung: the bound did not fire").
- `persistant-sqlx` now depends on `tokio` (features `sync`, `time`) without the `sqlite` feature; sqlx already pulled it in.

## 0.8.19

### Changed
- Recorded the disk-full result of run 39325665814 (main 9a9fad2, guest ext4): SQLite and PostgreSQL FAIL LOUD with every acknowledged key kept; **MySQL STALLS** (no error, no answer, InnoDB waits for space) with every acknowledged key kept once space was freed. The kv `choose` declarations for `sqlite-full`, `postgres` and `mysql` say so, and "PG/MySQL ENOSPC" is no longer in their unverified lists. The PostgreSQL log line `duplicate key ... persistant_kv_batch_pkey` is the batch-lock row insert in `KvOnRelational::open`, tolerated on purpose, once per store open. Docs and declaration strings only.

## 0.8.18

### Fixed
- Machine durability harness, disk-full step for PostgreSQL and MySQL (run 39306449594 gave "LOST AN ACK" with no evidence, then a MySQL setup failure with no output). Reproduced in an Ubuntu 24.04 container:
  - MySQL setup failed because the 130 MB volume was too small for the new cluster (~110 MB; "The table 'help_topic' is full"). Volumes are now 80 MB (PostgreSQL) and 200 MB (MySQL), made without reserved blocks, with a FILLER file that leaves exactly 12 MB for the puts (a 56 MB fill took 20 minutes). `pa-small-db` now names its step on stderr and reports the step and exit code it failed at, keeps one image per server, and unmounts lazily.
  - PostgreSQL PANICs when a WAL write fails, and on a full volume crash recovery needs room too: with 2 MB freed it stayed down ("could not write file base/16385/pg_filenode.map.tmp"), with ~10 MB freed it recovered. The ballast is now 8 MB and `pa-small-db ... free` starts the server again, as an operator would.
  - `machine_vm check` no longer reports an unreadable store as lost: it retries the connection for two minutes (exit 5 = cannot connect), puts a 60 s limit on each read, and reports read errors as `unreadable` (exit 6). Exit 1 (lost) names the first missing keys, held and unreadable counts. `run.ps1` retries 5/6/124 up to 4 times, only prints LOST AN ACK when the output names missing keys, otherwise UNVERIFIED, prints the writer's outcome, progress-line count and exit code of every guest step, dumps guest state after each PostgreSQL/MySQL step, and records a failing backend step as ERROR instead of aborting the run without a result file.
  - Container run of the full sequence: PostgreSQL failed loud (ENOSPC on pg_wal) after 21399 acknowledged, 1000 of them re-read and held; MySQL stalled (60 s, binlog) after 7108 acknowledged, held after the ballast was freed; a check past the acknowledged range printed `lost 191 of 7300 ... first missing keys [7109, ...]` (exit 1); an unreachable server printed exit 5. Not yet run under Hyper-V.

## 0.8.17

### Fixed
- Machine durability harness: the PostgreSQL disk-full step ran 83 minutes with no output until the job timeout (run 39288280654) because the host waited on the writer with no limit. `machine_vm enospc` now prints `progress N` every 500 acknowledgements; `run.ps1` reads them as they arrive and ends the step as **STALLED** after 120 s without one (or 25 min in all), dumps guest state (pg_isready, ps, df, database logs, dmesg), frees the ballast and checks every key up to the last progress line. STALLED is its own verdict, never "failed loud": a client that waits on a full disk is worse than one that is told. Job timeout 90 -> 45 minutes. The reader was tested against a fake writer for the loud, stalled, MySQL-stalled and never-failed outcomes. Cause of the PostgreSQL hang is not yet known (a slow fill and a real hang look alike without the progress lines).

## 0.8.16

### Fixed
- Machine durability harness: the guest copy of `pa-small-db.sh` had every letter `r` deleted (run 39282354886): the CR-stripping `tr -d '
'` lost its backslash between PowerShell and ssh. Carriage returns are now stripped on the host, and the guest's copy must match the host file's SHA-256 or the run stops. 0.8.15's container pre-test ran the script directly, so it never exercised this path. PostgreSQL/MySQL ENOSPC still has no verdict.

## 0.8.15

### Added
- Machine durability harness: disk-full (ENOSPC) check for PostgreSQL and MySQL. `scripts/machine-durability/pa-small-db.sh` mounts a fresh small ext4 volume over the server's data directory in the guest; `run.ps1` fills it with puts, frees a ballast file, waits for the server and checks every acknowledged key. `machine_vm enospc` now treats a put with no answer in 60 s as a failure ("stalled"). Guest script and binary were exercised in an Ubuntu 24.04 container first (PostgreSQL 16: failed loud with ENOSPC after 31158 acknowledged, all present; MySQL 8.0: stalled on a full binlog after 84325 acknowledged, all present after the ballast was freed). Not run on Hyper-V yet; no declaration changes.

## 0.8.14

### Added
- `SqliteBackend::open_durable(path)`: WAL and `synchronous=FULL` set explicitly on every pooled connection. It reports as `sqlite-full`, so `kv-on-relational:sqlite-full` is declared `Machine` in the kv `choose` table (verified: guest power loss on ext4, run 39178436801; same unverified list as before).

### Changed
- `kv-on-relational:sqlite` (the default `open`) is declared `Process` and no longer carries `verified = Machine`: it does not assert `synchronous`, and the power-cut run covered FULL only. A consumer that needs `Machine` opens with `open_durable`.

## 0.8.13

### Changed
- ROADMAP: row 1a, an explicit `synchronous=FULL` SQLite profile that declares `Machine` (the default stays `Process`). Docs only.

## 0.8.12

### Added
- `kv.durability.machine` in the vocabulary, and `Declared.verified = Machine` for `kv-on-relational` on SQLite (`synchronous=FULL`), PostgreSQL and MySQL: run 39178436801 (main 02f86a7) cut a Hyper-V guest's power after 300 acknowledged writes, 5 cycles each, and every acknowledged key was present; the `synchronous=OFF` control lost every key in 4 of 4 cycles; SQLite disk-full failed loud with its acknowledged keys kept. The `choose` log names the test that verified a class. Declared classes are unchanged (SQLite stays `Process`). Not verified, and said so in the declaration: host power loss, NTFS, disk cache, torn sectors, PostgreSQL/MySQL disk-full, remote object stores. The relational and blob tables are unchanged (no relational-level or blob-level test).

## 0.8.11

### Fixed
- `build-image.ps1`: create `id_pa` with an empty passphrase under PowerShell 7 too, and replace an existing key that has one. `ssh-keygen -N '""'` from PowerShell 7 sets the passphrase to two quote characters, so the guest accepted the key and the client then could not sign under BatchMode: "Permission denied (publickey)" (run 39177227624). The image must be rebuilt (the guest holds the old public key).

## 0.8.10

### Fixed
- `build-image.ps1`: grant the runner account read on `id_pa` itself and read the ACL back (`ssh-keygen` leaves an owner-only ACL with inheritance removed, so the directory grant never reached the key and the runner's `Copy-Item` was denied; run 39176940159). The account is the `-RunnerAccount` parameter, default `gh-runner`.

## 0.8.9

### Fixed
- Machine harness (`run.ps1`): run SSH with a copy of the key owned by the runner account with an ACL for that account only (OpenSSH on Windows refuses a key owned by another account, exit 255, which was retried until the timeout; the guest had an address but no SSH was ever accepted, run 39173418373). On a timeout it now also prints the last ssh output, `Test-NetConnection` to port 22, the firewall profiles and `ssh -vvv`.

## 0.8.8

### Fixed
- `build-image.ps1`: `chmod 644` the qcow2 copy before `qemu-img resize` (the copy kept the read-only mount's mode: "Permission denied").

## 0.8.7

### Fixed
- Machine harness (`run.ps1`): reach the guest by the address found from its VM adapter (KVP, else the host neighbor table by MAC), not by DNS name: the Default Switch kept the provisioning VM's stale record and port 22 timed out (first runner run 39167386852). On failure it prints VM state, adapter, neighbors and DNS. The guest image now installs `linux-cloud-tools-virtual` (hv_kvp_daemon); the image needs a rebuild for KVP, the old image does not work at all: its netplan is tied to the provisioning NIC's MAC (PM probe: a clone's MAC never appears on the network), so the image is rebuilt with a name-matched DHCP netplan and cloud-init network config disabled.

## 0.8.6

### Fixed
- Tests only: four SQLite test files (`decode_types`, `kv_on_sqlite`, `rel_speed_choose`, `sweep`) named their database file by clock nanos plus pid, so two tests starting in one clock tick would share a file. Each name now carries a process-wide counter. A candidate cause of one Windows CI failure in `rel_speed_choose` (not reproduced locally).

## 0.8.5

### Added
- The `Machine` durability harness (PM [TASK], board 229 ruled A): `scripts/machine-durability/` (guest image build from a pinned, checksummed Ubuntu 24.04 cloud image; a PowerShell driver that boots a Hyper-V Gen2 guest, writes over SSH, runs `Stop-VM -TurnOff` after N acknowledgements, boots and checks every acknowledged key; a known-bad control that must lose; a disk-full check that must fail loudly), the guest binary `examples/machine_vm.rs`, and `.github/workflows/machine-durability.yml` for the self-hosted runner (push to main, nightly, dispatch). No library change; no backend is marked `Machine`-verified yet.

## 0.8.4

### Added
- `docs/COMPILE-TIME-SQL.md`: design for re-exporting UniSQL's compile-time macros with catalog-based checks (schema snapshots from migrations or a live catalog, the checks that need no server and the ones never claimed, the `persistant::sql!` API and features, `persistant-migrate snapshot`, the road to typed rows). Also records that `sqlx::query!` cannot run on PersistAnt's transaction types today. `ROADMAP.md`: five rows. No code change. `matrix/opendal.tsv` regenerated: OpenDAL floated from 0.59.4 to 0.59.5 (no lockfile), which the matrix check records.

## 0.8.3

### Added
- `MysqlBackend::verify_fresh(&[Migration])`: installs the migrations in an empty scratch database (`persistant_fresh_*`) on a one-connection pool, then drops it, pass or fail. The target's ledger and tables are never touched. Needs `CREATE`/`DROP` on the scratch name; a migration naming another database escapes it (documented).
- `persistant-migrate verify-fresh` now works on MySQL and MariaDB.
- `persistant-migrate` prints its target (scheme, user, host, port, database; never the password) to stderr on every run, and refuses a target that is not loopback, a unix socket or an SQLite file unless `--allow-remote` is given (exit 9). A `host=` query parameter is honoured when judging, and an unreadable URL counts as remote.
- Tests `tests/cli_server.rs`: the binary end to end on PostgreSQL and MySQL/MariaDB (up, status, exit codes 3 and 4, verify-fresh leaves no scratch object and leaves the target alone, a broken set fails with 6). CI runs them in the PostgreSQL, MySQL and MariaDB jobs.

## 0.8.2

### Added
- `persistant-migrate`, a command-line tool over `Migrates` (a new crate and binary): `up`, `status` and `verify-fresh` for a directory of `NNN_name.up.sql` files (or plain `NNN_name.sql`), on PostgreSQL, SQLite, MySQL and MariaDB by URL scheme. `.down.sql` files are read and checked for pairing but never run (`Migrates` has no undo). `status` compares the directory with the ledger's checksums; exit codes tell a changed migration (3), a database newer than the directory (4), a dirty migration (5) and a failed one (6) apart. `verify-fresh` is offered for PostgreSQL and SQLite, not MySQL/MariaDB. The URL may come from `PERSISTANT_DATABASE_URL`.
- `PgBackend`, `SqliteBackend` and `MysqlBackend` gained `applied_checksums()`: the ledger's `(version, checksum)` rows, so a caller can compare a directory with a database without applying anything.

## 0.8.1

### Changed
- `ROADMAP.md`: a queued row for a migration CLI (up, status, verify-fresh over `.up.sql`/`.down.sql` files). No code change.

## 0.8.0

### Changed (breaking)
- `MigrateReport` gained `checksums: Vec<(i64, Vec<u8>)>` (C1): the ledger's checksum (SHA-384 of the migration text, sqlx's own) for every version applied after the run, ascending. A consumer that builds a `MigrateReport` by literal must add the field; a backend whose ledger keeps no checksum leaves it empty.

## 0.7.14

### Added
- Row decoder coverage (`docs/DECODER-AUDIT.md`): `uuid`, `numeric`/`DECIMAL`, dates and times, `timestamptz`, `json`/`jsonb`, `inet`/`cidr`, `ENUM`, unsigned integers and `YEAR` are carried (PostgreSQL, MySQL, MariaDB) as canonical text or `Int`, where they used to fail. Everything else still fails loud with the type name. A test per type per backend (`tests/decode_types.rs`).

### Fixed
- `real` / `FLOAT` read back as the shortest decimal (`0.1`), not the widened `0.10000000149011612`.
- MySQL/MariaDB `TINYINT(1)` holding a value other than 0 or 1 was read as `true`; it is now that integer (a byte of 128 or more is refused, since signed or unsigned cannot be told).
- PostgreSQL `char(n)` was refused although the decoder meant to carry it.
- MySQL `BIGINT UNSIGNED` above `i64::MAX` is refused naming the value, never wrapped.

## 0.7.13

### Added
- `persistant_relational::sweep(db, table, column, cutoff, batch, max_batches)` (C4): delete rows of a consumer's own table whose timestamp column is below a cutoff, in bounded autocommit batches (PostgreSQL by `ctid`, SQLite by `rowid`, MySQL and MariaDB by `DELETE .. LIMIT`). A mechanism only: no default cutoff, schedule or table list; the retention policy belongs to the table's owner. Names must be plain identifiers; `NULL` timestamps are never deleted; a row refreshed past the cutoff by a concurrent writer is kept. Tested on PostgreSQL, SQLite, MySQL and MariaDB.


## 0.7.12

### Added
- `PgBackend::verify_fresh(&[Migration])` (C2): installs the migrations on an empty scratch schema, in order, then drops it. The target's ledger and tables are never touched; success and failure both leave no schema behind. Migrations naming another schema, or creating cluster-wide objects, escape the scratch schema (documented, not detected).

## 0.7.11

### Added
- `persistant_sqlx::migrations_from_sqlx(&Migrator)`: run `sqlx::migrate!("./migrations")` (or a `Migrator::new(path)`) through `Migrates::migrate`. Same `_sqlx_migrations` ledger and SHA-384 checksums, so a database migrated by `sqlx migrate run` is accepted as it is; down halves are dropped. A `Migrator` that sqlx would run differently (`no_tx`, another ledger table, locking off) is refused with `FromSqlxError`, never run differently.
## 0.7.10

### Added
- docs: ROADMAP section "Adoption candidates: Humboldt" (what a concrete-pool consumer needs; two of its asks are already built). No code change.

## 0.7.9

### Added
- docs: `docs/DESIGN-DURABILITY-MACHINE.md`, a design note for verifying `Machine` (power-loss) durability: options on this PC, a recommended Hyper-V harness, what it proves and what stays unverified. No code change.

## 0.7.8

### Changed
- docs: ROADMAP no longer lists concurrent-migrator tests as still to build; `rel.migrate.concurrent` has been a leaf since 0.7.4, and MariaDB runs it since 0.7.7.

## 0.7.7

### Added
- **MariaDB certifies like MySQL.** `KvOnRelational` translates to UniSQL's `mariadb` target (it refused with "no translation to mariadb yet"), and `kv_on_mysql` runs against MariaDB 11.4 when `PERSISTANT_TEST_MYSQL_FLAVOR=mariadb` (new CI job `mariadb`): every kv, relational and migration leaf is on except `rel.migrate.ddl_atomic`, as on MySQL.

### Fixed
- MariaDB's `ER_CONSTRAINT_FAILED` (4025) is a `CheckViolation`; only MySQL's 3819 was.
- Two simultaneous migrators on MariaDB no longer race (`Duplicate entry '2' for key 'PRIMARY'`): sqlx takes its migration lock with `GET_LOCK(.., -1)`, which MariaDB rejects with NULL, which sqlx ignores. `MysqlBackend::migrate` now takes its own finite `GET_LOCK` first on MariaDB.

## 0.7.6

### Added
- **`MysqlBackend::connect_with(secret, &ConnectOptions)`: a typed TLS policy for MySQL and MariaDB**, the same `TlsPolicy` and `ConnectOptions` as PostgreSQL (`Disable`, `Prefer`, `Require` by default, `VerifyCa`, `VerifyFull`; `VerifyFull` is sqlx's `VERIFY_IDENTITY`). The policy overrides the URL's `ssl-mode`. `MysqlBackend::connection_is_encrypted` asks the server (`Ssl_cipher`). `tests/tls_mysql.rs` runs against a TLS server and a `--tls-version=` (no TLS) server started by `.github/scripts/mysql-tls.sh`.

### Changed
- persistant-kv now takes unisql 0.24.11 (was 0.24.7), which no longer demands a text collation for a BIGINT column in `DO UPDATE .. WHERE`.

## 0.7.5

### Changed
- **`rel.upsert_where` is now on for MySQL and MariaDB.** They have no `ON CONFLICT .. DO UPDATE .. WHERE`, but `ON DUPLICATE KEY UPDATE n = IF(t.n < VALUES(n), VALUES(n), t.n)` holds the row back the same way. The leaf and `speed.rel.upsert_where` spell it that way for those dialects; the known-bad control (the unconditional `VALUES(n)`) is still caught. Checked on MySQL 8.4.11 and against unisql 0.24.9's `UpsertWhereAsIf` output, which is the same statement. MariaDB is not run here. Only `rel.migrate.ddl_atomic` stays off on MySQL. `bench/sqlx-rel-speed.tsv` regenerated: MySQL now has a number.

## 0.7.4

### Added
- **`rel.migrate.concurrent`**: two migrators started at once against one database apply each migration exactly once between them and both finish cleanly; none runs twice.

### Fixed
- **`MigrateReport.applied` was wrong under concurrency** (found by the new leaf, on MySQL and SQLite): the ledger was read before and after the run *outside* sqlx's lock, so two simultaneous runs each reported every version as theirs. It is now read on the migrating connection inside the lock. The migrations themselves always ran once. SQLite: sqlx takes no lock there, so runs through one `SqliteBackend` (and its clones) now take turns on an in-process mutex; a second *process* migrating the same file is not excluded. `persistant-sqlx` gains an optional `tokio` (sync) dependency under its `sqlite` feature.

## 0.7.3

### Added
- **Two relational speed workloads**: `speed.rel.savepoint` (in one open transaction, `SAVEPOINT`, an `INSERT`, `ROLLBACK TO SAVEPOINT`: three statements per sample) and `speed.rel.upsert_where` (`INSERT .. ON CONFLICT (id) DO UPDATE .. WHERE` over 100 rows, always taking the update branch, each result read back). Each needs the contract leaf of the same name (`rel.savepoint`, `rel.upsert_where`), so MySQL gets an excluded row for `upsert_where`, not a number. `Workload::ALL` is now six. `bench/sqlx-rel-speed.tsv` regenerated.

## 0.7.2

### Added
- **Migrations as leaves**: `persistant_relational::Migrates` (`migrate`, `applied_versions`) and `migrate_conformance`, five leaves on a **disposable ledger** (a scratch schema, a scratch database or a fresh file; each leaf drops the ledger and its own tables first and last): `rel.migrate.apply` (ordered, ledger lists them, a second run applies nothing), `rel.migrate.checksum` (an edited applied migration is refused), `rel.migrate.newer` (a program older than the database is refused), `rel.migrate.atomic` (a failing migration leaves none of its row changes, keeps the migrations before it, and is a classified failure), `rel.migrate.ddl_atomic` (the same for tables it created). Known-bad control: `Defect::NoMigrationLedger`, a migrator with no ledger, checksum or transaction. `Migration`, `MigrateError` and `MigrateReport` moved into `persistant-relational` and are re-exported by `persistant-sqlx` under the same names. `Migrates` is implemented for PostgreSQL (as before), and now MySQL and SQLite. **`rel.migrate.ddl_atomic` is off on MySQL**: DDL commits implicitly.

### Fixed
- **A failed migration no longer wedges the next run.** sqlx takes a session advisory lock for a migration run and does not release it when a migration fails; the pooled connection kept it, and the next run, on another connection, waited for it forever (MySQL 8.4: `GET_LOCK(.., -1)` still waiting after 550 s; PostgreSQL held it too and only escaped because the pool handed back the same session). Migrations now run on one connection, and after a failure that session is closed rather than returned to the pool (`run_direct`, sqlx's own route round the `Acquire` lifetime error). Regression test with a timeout on PostgreSQL and MySQL.

## 0.7.1

### Added
- **Relational kind, stages 1, 3 and 4 of backend benchmarking** (`persistant_relational::{contract, speed, choose}`), on the same core types as kv and blob. `contract::check` decides eligibility from a leaf subset (`conformance::certify_leaves`, new; `certify` is now `certify_leaves` over every leaf). `speed` has four workloads (`speed.rel.insert`, `select_by_key`, `update`, `transaction`), each timed only on a backend that passes the leaf it depends on (`rel.values` or `rel.transaction`); the rest are excluded with the reason, never ranked. `choose` ranks by a workload within an environment and excludes by leaf or by declared durability. Relational durability is **declared and not verified** (`verified: None` for SQLite, PostgreSQL and MySQL); no power-loss test exists.
- `examples/rel_speed` and `bench/sqlx-rel-speed.tsv` (one dated run on a developer machine: Windows, PostgreSQL 16 and MySQL 8.4 in Docker Desktop, so their figures include the VM network). CI times SQLite and uploads the data; nothing compares a figure to a constant.

## 0.7.0

### Breaking
- **`RelationalBackend::begin` takes an `Isolation`** (`ReadCommitted`, the default, `RepeatableRead`, `Serializable`). Migration: pass `Isolation::ReadCommitted` (re-exported from `persistant_relational`) to every `begin()` call to get exactly what `begin()` gave before; implementers of the trait add the parameter and map the level (PostgreSQL `BEGIN ISOLATION LEVEL ..`, MySQL `SET TRANSACTION ISOLATION LEVEL ..; START TRANSACTION`; SQLite ignores it, every transaction there is `BEGIN IMMEDIATE`). `Isolation` is `non_exhaustive`: a backend that does not know a level returns `RelError::Unsupported`.

### Added
- `rel.isolation.serializable` (relational leaf 11). PostgreSQL: of two concurrent serializable transactions that each read both rows and write a different one, one is refused with a serialization failure; the control is that at read committed both commit. MySQL: a plain read in a serializable transaction locks the row (another connection's `FOR UPDATE NOWAIT` is refused, and is not at read committed or after the transaction). Known-bad control: `NoTransactions`. **Off on SQLite**, with the reason in the row: `BEGIN IMMEDIATE` holds the write lock so the probe cannot run.
- `rel.isolation.repeatable_read` now begins its transaction at that level instead of asking inside it, so it is **on for MySQL** too (it was off in 0.6.8); still off on SQLite.

## 0.6.8

### Added
- `rel.isolation.repeatable_read` (relational leaf 10): inside one transaction a row read twice reads the same and a table counted twice counts the same, though another connection committed an update and an insert in between. Known-bad control: `NoTransactions`. **Off on MySQL, honestly**: its `begin` starts READ COMMITTED (the deadlock fix of 0.6.4) and InnoDB cannot raise the level inside an open transaction; reaching it needs a level argument on `begin` (breaking, 0.7.0). PostgreSQL asks for the level with `SET TRANSACTION`.
- Not yet covered: `rel.isolation.serializable` (needs the same level argument to be probed on MySQL).

## 0.6.7

### Added
- `rel.upsert_where` (relational leaf 9): `INSERT .. ON CONFLICT (..) DO UPDATE .. WHERE ..` inserts when absent, updates only when the condition holds. Known-bad control: the same statement without the `WHERE` must fail it. **Off on MySQL and MariaDB, honestly**: they have no such clause (the MySQL matrix records `missing`). PostgreSQL and SQLite rows come from CI.

### Fixed
- Conformance harness: the `NoTransactions` known-bad control ran `SAVEPOINT` straight on the pool, which on SQLite opened a transaction on one pooled connection and never closed it, so any later leaf that wrote hit `SQLITE_BUSY`. Savepoint statements are now no-ops in that control. Latent since 0.6.5 (no later leaf wrote on SQLite until `rel.upsert_where`).

## 0.6.6

### Added
- `rel.row_lock` (relational leaf 8): a row locked `FOR UPDATE` by an open transaction cannot be locked by another connection (probed with `SKIP LOCKED`) until the transaction ends. Known-bad control: `NoTransactions`. **Off on SQLite, honestly**: it has no row locks (a write locks the whole database), so the SQLite matrix records the leaf as `missing`. PostgreSQL and MySQL rows come from CI.

## 0.6.5

### Added
- `rel.savepoint` (relational leaf 7): inside a transaction, `ROLLBACK TO SAVEPOINT` undoes the work after the savepoint, keeps the work before it, and the transaction still commits. Known-bad control: the `NoTransactions` defect must fail it. Verified locally; CI supplies the matrix rows.

## 0.6.4

### Fixed
- **`KvOnRelational::batch` on PostgreSQL and MySQL: racing committers could be handed "deadlock detected"** instead of `Ok(false)`.
  Found by the Synapse mailbox consumer check (8 threads, unseeded version key, `Require(ver, None)` first). Cause: a `Require` of an *absent*
  key locks nothing that excludes a second batch (a gap lock on MySQL, an in-flight insert on PostgreSQL), so racing batches took row locks in
  opposite orders and the server aborted one. Now every batch first locks one row of a new table `persistant_kv_batch` (created by `open`, one row,
  additive), so batches queue instead of deadlocking; SQLite already excludes. A batch the server still aborts as a deadlock or serialisation victim
  is run again (up to 16 times), since it was rolled back whole. Trade-off: batches on one database now run one at a time.

### Added
- `kv.batch_atomic.absent_race` (17th kv leaf): concurrent read-then-batch committers racing for a version key that starts absent; every batch is
  answered true or false, none gets an error, and no commit is lost. Red before the fix on PostgreSQL 16 and MySQL 8.4 (0/5 runs, "deadlock detected"),
  green after. Known-bad control: `BatchIgnoresRequire`.


## 0.6.3

### Added
- `rel.isolation.no_dirty_read` (relational leaf 6): a write inside an open transaction is invisible to
  another connection until it commits, and visible after. Known-bad control: the `NoTransactions` defect
  (statements run at once) must fail it. Verified on SQLite locally; PostgreSQL and MySQL rows come from CI.
  Not yet covered: the other `rel.isolation` levels (repeatable read, serializable).


## 0.6.2

### Fixed
- **The `kv_on_mysql_certifies_every_leaf_on` flake: found and fixed.** Reproduced on the 8th of 8
  full-suite runs (and not in 14 isolated runs, nor 6 under a 40-process CPU burn): the failing leaf was
  `kv.ordered_scan_values`, "scan with values returned keys [... s/gone ...]": the 100 ms entry was
  still listed 250 ms later. Cause: that leaf waits on the clock but was the one wall-clock leaf missing
  from the three-attempt retry list. The trigger is real and measured: the clock inside the Docker
  Desktop VM running MySQL stepped by up to about 1.2 s against the host (offset -1209..+862 ms over six
  minutes, round trips under 20 ms), so the server sometimes judges an entry unexpired after the client
  slept past its time to live. `Leaf::wall_clock` now decides the retry set and is an exhaustive match,
  so a new leaf must choose; a test shows a one-attempt clock step is retried and a real defect (which
  fails every attempt) is not hidden, and fails if the leaf is dropped from the set (mutation-checked).
  This is a property of that test environment, not of MySQL or of PersistAnt; a Linux CI runner shares
  its kernel's clock with its service containers and has not shown it.

## 0.6.1

### Added
- `persistant-relational`: `conformance::{certify, run_leaf, Leaf, Defect, Defective}`, the relational
  kind's own leaves, so it has something to measure: `rel.values` (integers, text, bytes, floats and
  NULL round-trip), `rel.transaction` (commit visible, rollback and drop-without-commit not),
  `rel.error.unique`, `rel.foreign_key`, `rel.check_constraint` (violations come back as the typed
  `RelError`, not as opaque backend text). **Known-bad controls are real:** a `Defective` wrapper makes
  the same target have no transactions, lose bytes or hide error kinds, and the two constraint leaves
  are also run on a schema created *without* the constraint, which must be caught accepting the bad
  row. **The positive control is weaker than in kv and blob** (there is no reference relational engine
  to pass against): it is only that the leaf's schema could be created on the target. Statements are
  written per dialect (a probe of the dialect, not of translation); booleans are left out because how
  a driver returns one is a documented difference.
- All five are on for PostgreSQL 16, SQLite and MySQL 8.4; the three `matrix/sqlx-*.tsv` files gain five
  rows each (21 per backend) and the matrix examples emit them.

### Fixed
- `persistant-sqlx` (PostgreSQL): a `NULL` parameter was sent with type `text`, which the server rejects
  for a `bigint`/`bytea` column (`expression is of type text`) on any pooled connection that had not
  already prepared the statement: a failure that depended on which connection the pool handed out. A
  `NULL` is now sent with an unspecified type and the server infers it. Found by `rel.values` (it first
  passed alone and failed in the matrix run); pinned by `a_null_parameter_works_on_every_pooled_connection`,
  which fails without the fix.

### Changed
- The PostgreSQL and MySQL integration tests take a lock: they share one server and the kv table's
  fixed key prefixes, so two tests certifying at once deleted each other's keys.

## 0.6.0

### Breaking (pre-1.0: the second number)
- The kind-agnostic half of benchmarking moved to `persistant-core`: `bench` (`Environment`, `Stats`,
  `Outcome`, `SpeedRow`, `SpeedTable`, `Comparison`, `compare`, the data file) and `choose`
  (`Durability`, `Declared`, `Need`, `Candidate`, `Choice`, `choose`, `LeafFailure`, `Eligibility`).
  `persistant_kv::speed` re-exports the types it had, so those paths still work. What changed shape:
  `persistant_kv::choose::Need` names leaves and the ranking workload by **id string** (build one with
  `persistant_kv::choose::need(&[Leaf], min, Workload, env)`), a `Candidate`'s eligibility is the core
  `Eligibility`/`LeafFailure` (string leaf ids), and `Why::Contract` carries `LeafFailure`s.
  `persistant_kv::speed::ParseError` is now the core `SpeedParseError` under the old name.

### Added
- `persistant-blob`: `contract::{check, evaluate}`, `speed::{run, Workload}` (write, read, stat, list,
  create_exclusive, replace_atomic, append; 4 KiB values) and `choose::{choose, candidate, need,
  declared_durability}`: stages 1, 3 and 4 for the blob kind, on the shared core types. Only a backend
  that passes the leaf a workload needs is timed; the rest are listed as excluded. **Blob durability is
  declared and not verified at all** (`verified: None`): the kv process-kill test exercises the same
  services through `KvOnBlob`, but there is no blob-level one, and there is no power-loss test.
- `persistant-blob`: `conformance::certify_leaves` (certify a chosen set of leaves, controls included),
  as kv already had.
- `persistant-opendal --example speed` also times the blob kind on the memory and fs services; a test
  runs the blob contract, speed and `choose` together over real OpenDAL services (and allows for a
  case-insensitive filesystem excluding the fs service, as the contract intends).
- Not covered, stated: the relational kind (no leaves of its own yet: nothing to measure, owner the
  PersistAnt lane), blob concurrency in speed, and value sizes other than 4 KiB.

## 0.5.8

### Added
- `persistant-kv`: `durability::{certify_process, verify, child_loop, maybe_run_child, cleanup}` and the
  leaf `kv.durability.process`: a writer process is killed hard after it has acknowledged writes, the
  store is opened again, and every acknowledged key must be there. **Known-bad control:** the same child
  in `AckFirst` mode acknowledges before writing and must be caught losing keys; the leaf is recorded on
  only if the honest run lost nothing *and* the lying run was caught. Verified now: SQLite (file),
  OpenDAL fs, PostgreSQL and MySQL (a killed *client*; the server outliving it). **A memory store
  fails it, as it must** (SQLite `:memory:` and the OpenDAL memory service are in the tests as the
  harness's own negative control).
- `choose::Declared::verified`: the highest class a test has checked. The log line for a ranked
  backend now says `verified: up to process by kv.durability.process` or `verified: no`. **`Machine`
  (power loss, an OS crash) is still only declared** for PostgreSQL and MySQL: a process kill leaves
  the OS and its caches intact, and a test of the rest needs a VM hard reset. A test asserts nothing
  is marked verified above `Process`.
- Harness tests (`harness = false`, the binary re-runs itself as the writer): `durability_sqlite`,
  `durability_fs` (OpenDAL), `durability_postgres`, `durability_mysql`; the last two run in the CI jobs
  that provide the server.
- Not covered, stated: the `Machine` class, durability of the blob kind, and a crash inside a `batch`.

## 0.5.7

### Changed
- `ROADMAP.md`: status of the benchmarking row (kv: all four stages) and the queued items in order
  (durability verification, `choose` for blob and relational, the Synapse mailbox consumer check).
  No code change.

## 0.5.6

### Added
- `persistant-kv`: `choose::{choose, candidate, Need, Candidate, Choice, Entry, Durability,
  declared_durability}`, benchmarking stage 4: "best backend for X". The consumer states the contract
  leaves it needs, the minimum durability class (`Volatile` < `Process` < `Machine`), the one workload
  to rank by and the environment the ranking is for. The answer is the eligible backends fastest
  first, each with the measurement it was ranked on (date, image digest, re-run command) and its
  declared durability; the eligible ones with no measurement in that environment (listed, not
  hidden); and every excluded backend with its reason. Nothing eligible is `Err(NoneEligible)` with
  the exclusions: no silent fallback. `Choice::log_lines` is the decision as lines to log.
- **Durability is declared, not verified.** `declared_durability` is a table of what the backends
  document (PostgreSQL and MySQL defaults: `Machine`; SQLite file and OpenDAL fs: `Process`; memory:
  `Volatile`), each with its basis. There is no power-loss test yet (`blob.durable` is not built); a
  backend with no entry is excluded as unknown once the need asks for more than `Volatile`.
- Not covered, stated: ranking by more than one workload, concurrency-aware ranking, durability
  verification, and the blob and relational kinds (no `choose` for them yet).

## 0.5.5

### Added
- `persistant-kv`: `speed::{run, Workload, Environment, SpeedRow, SpeedTable, compare}`, benchmarking
  stage 3. Seven workloads (put, get, put_if_absent, compare_and_swap, batch, ordered_scan,
  ordered_scan_values) timed per operation in nanoseconds (nearest-rank p50/p95/p99, max). **Only a
  backend that passes the contract leaf the workload needs is timed**; the rest get an `excluded` row
  with the reason and are never ranked. Every row carries its `Environment` (CPU model and cores, OS,
  architecture, date, the server image digest when there is one) and the command that re-runs it;
  `compare` orders backends only within one environment, so a ranking never crosses machines. Results
  are a tab-separated data file that round-trips.
- Examples: `persistant-opendal --example speed` (MemoryKv, KvOnBlob over memory and fs),
  `persistant-sqlx --example speed` (SQLite; PostgreSQL and MySQL when their URLs are set),
  `persistant-kv --example speed_report` (prints per workload and environment, from the files).
- `bench/`: a first dated run on one machine (a Windows desktop; PostgreSQL and MySQL in Docker
  Desktop containers, so their round trip includes the VM). CI runs the same examples on its runner,
  uploads the files as artifacts and prints the report; nothing compares a figure to a constant.
- Not covered, stated: concurrency (every workload is one caller at a time), value sizes other than
  256 bytes, warm-up beyond the first operations, the lock and relational kinds (no leaves of their own
  yet: nothing to measure, owner PersistAnt lane), and the "best backend for X" API (stage 4).

## 0.5.4

### Added
- `persistant-blob`: `stats::{measure, LeafStats, Instrumented, OpCount}`, benchmarking stage 2 for the blob
  kind (same shape as `persistant_kv::stats`): each leaf run `runs` times with one attempt each, counted
  as passed / wrong / errored, with per-operation calls and errors by kind. Exact only if every run
  passed. Test over the OpenDAL memory service: the concurrency leaves it offers are exact on five
  runs; the append leaves it does not offer fail every run as `Unsupported`, not as a contract break.


## 0.5.3

### Changed
- `kv.ttl` runs with a 1000 ms time to live and a 1200 ms sleep (was 300/450). On a shared CI runner
  the 300 ms window was missed once by a correct backend (SQLite, 2026-10-16, passed on rerun). The
  window is wall-clock time on a loaded machine: a limit of the method, not a verdict on the
  backend, and now said so in `conformance`'s docs. The `kv.ttl` row is re-keyed in all four
  `matrix/*.tsv` files (metadata only; every row stays on).

## 0.5.2

### Added
- `persistant-kv`: `stats::{measure, LeafStats, Instrumented, OpCount}` and `conformance::run_leaf_once`:
  benchmarking stage 2 (ROADMAP.md). `measure` runs each leaf `runs` times, one attempt each (no retry), and
  reports passed / wrong / errored with the first failure; `Instrumented` counts per-operation calls and
  errors by kind. A leaf is exact only if every run passed; a fraction is never rounded up. No timing.
- Tests: on SQLite, PostgreSQL and MySQL the six sampled-concurrency leaves must be exact on five
  consecutive runs (real-time leaves are left out: a stalled CI machine would fail them for the wrong
  reason).

## 0.5.1

### Added
- `persistant-sqlx` (feature `mysql`): `MysqlBackend` (`connect`, `connect_secret`, `from_pool`);
  the dialect is read from the server (`MySql` or `MariaDb`). Transactions run at `READ COMMITTED`
  (under InnoDB's default REPEATABLE READ, concurrent insert-if-absent deadlocks on gap locks: the
  suite found it). Errors classified from MySQL error numbers. TLS policy is the URL's `ssl-mode`
  for now, not typed.
- `persistant-kv`: `KvOnRelational` over MySQL 8.4 through UniSQL 0.24.7. Keys are
  `VARBINARY(1024)` (bytewise; a longer key is rejected by the server), values `LONGBLOB`. A
  conditional insert-if-absent is two statements there (`claim_two_step`): sqlx connects with
  `CLIENT_FOUND_ROWS`, under which an upsert that changed nothing reports the same count as an
  insert (measured; `ROW_COUNT()` follows the same flag), so no single upsert can say whether it
  stored. Allowances: `LenientCast` and `UpsertAnyUniqueKey`; UniSQL's `UpsertWhereAsIf` is not used.
- `matrix/sqlx-mysql.tsv` and a CI job (MySQL 8.4 service container pinned by digest, read from the
  registry 2026-10-16) that regenerates and compares it: all 16 leaves certified, controls included.
- Not covered, stated: MariaDB (the dialect is detected, KvOnRelational refuses it), a typed TLS
  policy, keys over 1024 bytes.

## 0.5.0

### Added
- `persistant-sqlx` (feature `sqlite`, bundled library): `SqliteBackend` (`open(path)` / `from_pool`),
  WAL, a bounded wait for the write lock, write transactions as `BEGIN IMMEDIATE`, errors classified
  from SQLite's extended result codes. No server, so its tests cannot skip.
- `persistant-kv`: `KvOnRelational` over SQLite. The statements are still written once, in PostgreSQL
  SQL, and now translated by UniSQL 0.24.7 (`unisql`, from crates.io); PostgreSQL is passed through
  unchanged. Allowances granted, each with its reason in `translate.rs`: `LenientCast`,
  `ClockIsStatementStart` (SQLite reads the clock once per statement), `UpsertAnyUniqueKey`. The
  table definition is per dialect (DDL is outside UniSQL's IR). `SELECT ... FOR UPDATE` is dropped
  on SQLite, whose write transaction is already exclusive. SQLite older than 3.42 is refused
  (`unixepoch('now','subsec')`).
- `matrix/sqlx-sqlite.tsv` and a CI job that regenerates and compares it: all 16 kv/lock leaves
  certified on SQLite, controls included.
- **Breaking (pre-1.0 minor): the minimum Rust version is 1.94 for every crate** (was 1.91; UniSQL
  0.24 needs 1.94). CireSnave permitted raising it, 2026-10-08.
- Not covered, stated: MySQL/MariaDB (next), a shared in-memory SQLite database across connections
  (use a file, or a one-connection pool), crash safety of SQLite itself.

## 0.4.35

### Added
- `persistant-kv`: leaf `kv.batch_atomic.optimistic_sequence` (the Synapse mailbox pattern: read, yield,
  `batch([Require(read value), Put])`, retry on refusal). Callers must each get a distinct number and the
  counter must end at the number of increments. Known-bad controls: `BatchIgnoresRequire` and
  `BatchNotAtomic` (Synapse measured 74 duplicates of 100 without the `Require`s). `KvOnBlob` is
  `Missing`. Certified matrix rows 15 -> 16.


## 0.4.34

### Added
- `persistant-kv`: `contract::{check, evaluate, Eligibility, Exclusion}` and `conformance::certify_leaves`:
  backend benchmarking stage 1 (ROADMAP.md). A consumer names the leaves it needs; a backend that does
  not pass every one is `Excluded` with the reason per leaf (`Missing`, `Wrong`, `Untested`), never ranked
  slower. A required leaf with no test is excluded as `Untested`. Tests: correct backend eligible;
  `KvOnBlob` excluded as `Missing` for batch; a defective `MemoryKv` excluded as `Wrong`.

## 0.4.33

### Added
- `ROADMAP.md`: the backend benchmarking and ranking row (contract suite, statistics, speed, ranked
  `best backend for X` with evidence) and the SQLite/MySQL row. No code change.


## 0.4.32

### Added
- `persistant-kv`: `BatchOp::{Put { key, value, ttl }, Delete { key }, Require { key, expected }}`
  and `KvBackend::batch(&[BatchOp]) -> Result<bool>`: all-or-nothing. `Ok(false)` means a `Require`
  precondition was unmet and nothing changed. Also `ordered_scan_values`. Implemented by `MemoryKv`
  and `KvOnRelational` (PostgreSQL). `KvOnBlob` does not offer `batch`.
- Three conformance leaves: `kv.batch_atomic.rollback`, `kv.batch_atomic.concurrent_read`,
  `kv.ordered_scan_values`. Certified matrix rows 12 -> 15.
- Not covered, stated: the full `kv.batch_atomic` crash leaf stays off.

## 0.4.31

### Added
- `persistant-sqlx`: `TlsPolicy` (`Disable`, `Prefer`, `Require`, `VerifyCa`, `VerifyFull`, the last
  two with an optional root certificate file) and `ConnectOptions { tls, max_connections,
  acquire_timeout }`; `PgBackend::connect_with(&ConnectionSecret, &ConnectOptions)`. The policy
  **overrides** the URL's `sslmode`, so a connection string cannot weaken it. The default is
  `Require` and the default acquire timeout is bounded (10 s), so a saturated pool is an error, not
  a hang. `PgBackend::connection_is_encrypted()` asks the *server* (`pg_stat_ssl`).
- Tests against a real PostgreSQL with `ssl = on` (private CA, CA:FALSE leaf for `localhost`; a CA
  certificate presented as the server certificate is rejected by rustls as `CaUsedAsEndEntity`,
  which the first attempt hit): Require encrypts and the server agrees; Disable is plain (control
  for the probe); Require against a server without TLS fails while Prefer connects unencrypted;
  the policy overrides `sslmode=disable` in the URL; VerifyFull refuses an untrusted certificate and
  accepts the trusted one; an exhausted pool errors after its bound.
- `.github/scripts/postgres-tls.sh` and a CI step starting that server (pinned image, same waiver
  reason as the plain service container).
- Not covered, stated: client certificates (mTLS), certificate revocation, TLS to SQLite (n/a).

## 0.4.30

### Added
- `persistant-sqlx`: `PgBackend::migrate(&[Migration])` and `applied_versions()`: ordered,
  checksummed, per-migration-transaction schema migrations that **keep sqlx's own ledger**
  (`_sqlx_migrations`, written by sqlx's `Migrator`, SHA-384 of the text), so `sqlx migrate run`
  and `sqlx::migrate!` read the same table and Humboldt's version-and-checksum comparison keeps
  working (DESIGN-KINDS §5). Typed outcomes `MigrateError::{ChecksumMismatch, NewerThanProgram,
  Dirty, Failed}` and a `MigrateReport` of what was applied. Tests against a real PostgreSQL, in
  their own schema each: apply in order and re-run applies nothing, add a version to an existing
  ledger, an edited applied migration is refused, a database newer than the program is refused, a
  failing migration rolls back and names its version and error class, and the ledger's columns and
  checksums are sqlx's (SHA-384, computed independently in the test).
- sqlx feature `migrate` enabled; dev-dependency `sha2` for that independent check.

## 0.4.29

### Added
- `persistant-relational`: `ConnectionSecret` (an environment variable, a file, or a literal) and
  `Redacted`. A connection string is a reference or a value that prints as `<redacted>`; `Debug`
  never shows it and resolution errors name the variable or file, never a value (DESIGN-KINDS §5,
  "secret-handle connection config"). The text comes out only through `Redacted::expose`.
- `persistant-sqlx`: `PgBackend::connect_secret`. `connect(&str)` stays and its doc points at it.
  A test connects through a literal secret and asserts that neither the secret nor the backend
  prints the password.

## 0.4.28

### Added
- D7 option A (PM ruling 2026-10-14): `persistant` re-exports the new crates behind opt-in features,
  **off by default and additive**. `Store`, `Config`, `Need`, `Error` and the blocking facade are
  untouched. Features and modules: `core` as `persistant::capability`, `blob` as `persistant::blob`,
  `kv` as `persistant::kv`, `relational` as `persistant::relational`, `opendal-driver` as
  `persistant::opendal_driver`, `sqlx-driver` as `persistant::sqlx_driver` (needs Rust 1.94; the rest
  build on 1.91). `persistant-opendal` joins `[workspace.dependencies]`.
- `tests/reexports.rs` exercises each re-export under its feature; CI runs it with `--all-features`.
- `deny.toml`: allow `CDLA-Permissive-2.0`, the licence of `webpki-roots` (Mozilla's CA bundle as
  data) that sqlx pulls through `tls-rustls-ring-webpki`. A policy call for the PM at the gate: the
  alternative is sqlx's `tls-rustls-ring-native-roots` (system roots, no webpki-roots).

## 0.4.27

### Added
- `persistant-opendal::crash`: the crash half of `blob.replace_atomic` ("a crash leaves the whole
  old value or the whole new one"), decided by killing a process mid-replace. A host binary re-runs
  itself as the child (`PERSISTANT_CRASH_CHILD`); the parent kills it hard 12 times at varied delays
  and requires the key to hold the whole `A...` or the whole `B...` 16 MiB value. Controls: a std
  write-then-rename passes (positive), a slow in-place truncate-and-write is found torn
  (known-bad). Not covered, stated: power loss (`blob.durable`), since a process kill leaves the
  page cache intact.
- Findings recorded in `matrix/opendal.tsv`: `blob.replace_atomic` is **on** for `opendal-fs` with
  `atomic_write_dir`, and **missing** without it (the driver refuses: it writes in place).
- This is the prerequisite `docs/FACADE-MIGRATION.md` named for any matrix-gated `AtomicReplace`.
- `persistant-opendal` gains a normal dependency on `tokio` (feature `rt`), for the child's runtime.

## 0.4.26

### Changed
- `docs/FACADE-MIGRATION.md`, docs only, no code: a design note on moving the `persistant` facade
  onto the new crates. Gap table, the finding that a matrix-gated `AtomicReplace` needs a crash test
  nobody has written, three options, a recommendation (additive re-exports now; a crash test next;
  a breaking 0.5.0 only after it) and one decision (D7) for the PM and CireSnave.

## 0.4.25

### Changed
- `docs/DESIGN-KINDS.md`, docs only, no code: D4, D5 and D6 recorded as ruled/done; a status section
  (section 11) says what was built, nine things building found (each asserted in a test) and what
  is not built (SQLite/MySQL until UniSQL is published, migrations, TLS/secret handles, crash and
  power-cut tests, consumer migration); the opening no longer claims "no code".

## 0.4.24

### Added
- `persistant-kv`: `KvBackend::ttl_remaining` and `purge_expired` (default `Unsupported`), asked for
  by the auth-framework lane (`get_kv_ttl`, `cleanup_expired`; ADR-011). Certified leaves
  `kv.ttl_remaining` (a 10 s entry reports 0 < d <= 10 s, falling; none for no ttl, absent or
  expired) and `kv.purge_expired` (expired entries counted and gone, live ones and timed ones
  untouched). Known-bad: `TtlRemainingIsConstant`, `PurgeRemovesLive`.
  `KvOnRelational` gives both (`expires_ms - now`; one `DELETE`); `KvOnBlob` gives `ttl_remaining`
  but not `purge_expired` (deleting an expired entry races a concurrent put and a blob store has no
  compare-and-delete). Memory gives both.
- `persistant-core` vocabulary: `kv.purge_expired`.

### Changed
- `persistant-kv` conformance: the wall-clock leaves (`kv.ttl`, `kv.ttl_remaining`,
  `kv.purge_expired`, `lock.lease`, `kv.prefix_list`) get three attempts, so a stalled machine
  cannot turn a correct backend off; a defective backend fails every attempt.
- Recorded matrices regenerated for the two new leaves.

## 0.4.23

### Added
- `matrix/opendal.tsv` and `matrix/sqlx-postgres.tsv`: the recorded results of the certification
  (68 and 10 rows), written by `cargo run -p persistant-opendal --example matrix` and
  `cargo run -p persistant-sqlx --example matrix` (the second needs a server). Each row names its
  test and its environment (`os`, `arch`); there is no clock field, the commit date is the date.
  CI regenerates both on Linux and fails if they differ, so a backend or crate bump that changes
  what is certified shows up in review. The sqlx matrix is checked by the `relational` job against
  its pinned PostgreSQL.
- Tests that read the checked-in files through `persistant_core::Matrix` and ask the gate's
  question (`create_exclusive` on fs with and without `atomic_write_dir`; an uncertified version and
  an untested leaf are off).

### Changed
- `KvOnBlob` and `KvOnRelational` report their version as `<major>.<minor>+<backend version>`, so a
  patch release does not re-key every recorded row (pre-1.0 the second number is the major).

## 0.4.22

### Added
- Member crate `persistant-relational` (name held at 0.0.0; not published by this change): the small
  `RelationalBackend` / `RelationalTx` trait (execute, query, begin; dialect and server version),
  portable `Value` / `Row`, and `RelError` with typed classification (unique, foreign key, check,
  serialization, deadlock, unavailable, unsupported). No driver.
- Member crate `persistant-sqlx` (name held at 0.0.0; not published by this change): `PgBackend` over
  sqlx 0.9 (`rust-version = "1.94"`, only this crate), SQLSTATE classification, transactions that
  roll back on drop, and the concrete `PgPool` through `PgBackend::pool` for `sqlx::query!`.
- `persistant-kv`: `KvOnRelational<R>`, the KV kind native on a relational backend (D1, C+).
  PostgreSQL SQL, `BYTEA` keys (bytewise order, never collation), server-clock expiry, `ON CONFLICT`
  put_if_absent that reclaims an expired row, compare-and-swap and compare-and-delete as single
  statements. Certified against a real PostgreSQL 16.15: **all ten kv/lock leaves on**, including
  `lock.lease`, `kv.compare_and_swap` and `kv.compare_and_delete`. Any other dialect is refused
  until a translator is a published dependency.
- New known-bad control `ScanIsCaseInsensitive` (the collation trap) and two seed keys, `a/a` and
  `a/B`, which a case-insensitive or locale order puts the wrong way round.
- Finding, from the first Windows CI run of the above: **OpenDAL `fs` on a case-insensitive
  filesystem (Windows, default macOS) treats keys that differ only in letter case as one key**
  (and a file `a/B` collides with a directory `a/b`). `blob.read` and `kv.get` now include a
  case-distinct-keys check (known-bad control `KeysFoldCase` in both memory backends); on such a
  filesystem both are off with reason `wrong`, and the opendal tests probe the filesystem rather
  than assume the OS. The seed key `a/b/c` is now `a/c/d` so the harness does not itself provoke a
  file/directory clash.
- CI: a `relational` job runs the sqlx tests against a PostgreSQL 16 service container pinned by
  digest (`PERSISTANT_REQUIRE_DATABASE=1`, so a missing server fails instead of skipping), and an
  `msrv-sqlx` job; the workspace MSRV job excludes `persistant-sqlx`.

## 0.4.21

### Changed
- `persistant-kv` **(behaviour change, pre-publish)**: `KvOnBlob::new` is strict: `put` needs the
  blob store's `replace_atomic` and is refused where it has none, instead of silently writing in
  place. `KvOnBlob::with_in_place_put` opts in explicitly; `KvOnBlob::config()` reports
  `put_mode=atomic|in_place` so a row is recorded under the mode. Its backend name is now
  `kv-on-blob:<blob backend>` and its version `<crate>+<blob version>`, so rows for different
  blob stores do not collide.

### Added
- `persistant-opendal` tests run `KvOnBlob` over the real `fs` (three configurations) and `memory`
  services. Findings, asserted: fs without `atomic_write_dir`, strict, gives no `kv.put` and only
  the write-once `kv.put_if_absent` (every other leaf test starts by putting); with `with_in_place_put` it gives get/put/delete/ttl/put_if_absent/prefix_list/
  ordered_scan; fs with `atomic_write_dir` gives atomic put but not `kv.put_if_absent`; memory gives
  all but the compare-and-swap family. A strict `KvOnBlob` over fs gives `put` or `put_if_absent`,
  never both (README 0.4.7).

## 0.4.20

### Added
- `persistant-opendal`: `ProcessLock` (`lock.process`), an operating-system advisory file lock
  (`std::fs::File::try_lock`, std only), local only, released by the OS when its holder dies. The
  deciding test runs a child process (this test binary), locks in it, kills it hard and checks the
  lock is free; the known-bad control shows a marker file still excludes everyone after the same
  kill. It does not produce a matrix row from the library, because the child must be the host's own
  executable.

## 0.4.19

### Added
- `persistant-kv`: `KvBackend::compare_and_swap` and `compare_and_delete` (default `Unsupported`),
  `Lease` (`lock.lease`: acquire = `put_if_absent` with a time to live, renew = compare-and-swap,
  release = compare-and-delete; no approximation on a backend lacking them), and contract tests for
  `kv.compare_and_swap`, `kv.compare_and_delete` and `lock.lease` (free, held, renew, release,
  expiry and reclaim, a former owner cannot renew or release the new owner's lease, 16 concurrent
  reclaimers x 5 rounds; parameters in the row). Known-bad controls: `CasIgnoresExpected`,
  `CasNotAtomic`, `CadIgnoresExpected`, `CadNotAtomic`, plus `PutIfAbsentNotExclusive` for the lease.
- `persistant-core` vocabulary: `kv.compare_and_delete`.
- `put_if_absent` documents that an expired entry counts as absent.
- `KvOnBlob` offers none of the three: it has no compare-and-delete, so they are off (`missing`).

## 0.4.18

### Added
- Member crate `persistant-kv` (name held at 0.0.0 on crates.io; not published by this change):
  - `KvBackend` (get, put with optional time to live, delete; optional put_if_absent, prefix_list,
    ordered_scan, default `Unsupported`), `MemoryKv` with injectable defects, and
    `conformance::certify` for `kv.get`, `kv.put`, `kv.delete`, `kv.ttl` (real time: 300 ms window,
    450 ms sleep, expired entries invisible to get and listing), `kv.put_if_absent` (16 x 20
    sampled), `kv.prefix_list` (literal prefix incl. `%` and `[`, paging, expired absent) and
    `kv.ordered_scan` (bytewise order). Each leaf is on only with the target passing, the positive
    control passing and every known-bad control failing.
  - `KvOnBlob<B: BlobBackend>`, a wrapper type (not a blanket impl) storing a version/expiry
    envelope under `kv/<key>`. `put_if_absent` needs `blob.create_exclusive` and refuses a time to
    live and the reclaim of an expired entry (that needs compare-and-delete). Not offered:
    `compare_and_swap`, `take`, `batch_atomic`, `ttl_remaining`, `count`.
- `persistant-blob`: `concurrency` module (`Task`, `join_all`, `yield_now`), shared with the kv tests.

## 0.4.17

### Added
- `persistant-blob`: `BlobBackend::append` (default `Unsupported`) and the contract tests for
  `blob.append` (call order) and the new leaf `blob.append.concurrent_whole` (16 appenders x 10
  records of 64 bytes: every record whole, exactly once, none interleaved; parameters recorded in the
  row). Known-bad controls: `AppendOverwrites`, `AppendTearsRecords`.
- `persistant-core` vocabulary: `blob.append.concurrent_whole`.
- `persistant-opendal`: `append` through OpenDAL's append write. Findings asserted in tests: `fs` gives
  both leaves; the `memory` service does not support append (off, `missing`).

## 0.4.16

### Added
- `persistant-blob`: `BlobBackend::create_exclusive` and `replace_atomic` (default: `BlobError::Unsupported`,
  a new variant), the contract tests for `blob.create_exclusive` and the new leaf
  `blob.replace_atomic.concurrent_read` (a reader sees only whole old or whole new values), run by
  sampled concurrency with the trial parameters (`contenders=16;rounds=20`, `readers=4;replaces=40`)
  recorded in the row, and the defects `CreateNotExclusive` and `ReplaceTearsReads` as known-bad controls.
- `persistant-core` vocabulary: `blob.replace_atomic.concurrent_read`. `blob.replace_atomic` (which
  also requires crash safety) has no test yet and stays off.
- `persistant-opendal`: `create_exclusive` and `replace_atomic`. Findings, asserted in tests: `fs`
  without `atomic_write_dir` creates exclusively and does not replace atomically; `fs` with it replaces
  atomically and cannot create exclusively (the driver says `Unsupported`, README 0.4.7); `memory` gives both.

## 0.4.15

### Added
- Member crate `persistant-opendal` (name held at 0.0.0 on crates.io; not published by this change):
  `OpendalBlob` implements `persistant_blob::BlobBackend` for OpenDAL's `fs` and `memory` services,
  with no OpenDAL type in its public API. Rows are recorded under the fact `atomic_write_dir=set|unset`.
  `list` is emulated by a directory scan, as in `persistant::Store`.
- Tests run `conformance::certify` against real `fs` (with and without `atomic_write_dir`) and `memory`
  and a root that cannot hold keys (control: every leaf off, reason `missing`).

### Changed
- `persistant-blob` conformance: the literal-prefix test uses `%` and `[` (valid in a Windows file name)
  instead of `*`; the `ListPrefixIsGlob` defect is now `ListPrefixIsPattern`.
- The `persistant` crate is unchanged and does not yet use these crates.

## 0.4.14

### Added
- Member crate `persistant-blob` (name held at 0.0.0 on crates.io; not published by this change):
  the `BlobBackend` trait (read, write, delete, stat, list), `MemoryBlob` with injectable `Defect`s,
  and `conformance::certify`, which runs the contract tests for `blob.read`, `blob.write`,
  `blob.delete`, `blob.stat` and `blob.list` and returns matrix rows. A leaf is on only when the target
  passed, the reference backend passed (positive control) and every defect failed (known-bad control).
  Leaves needing concurrency, a crash or a power cut have no test yet and stay off.
- `persistant_core::Matrix::from_rows`.
- The `persistant` crate is unchanged; it does not yet implement `BlobBackend`.

## 0.4.13

### Added
- Workspace. New member crate `persistant-core` (the name is held on crates.io at 0.0.0; this version
  is not published by this change): `CapabilityId`, `Config` (`k=v;k=v`), `Row`/`Matrix` (the
  `docs/DESIGN-KINDS.md` section 2.4 matrix file: read, write, merge, and the gate's lookup where an
  unknown version, an unreadable config or no row is off and a matching `no` wins), `Verdict` (the four
  test outcomes; an `on` row needs both the positive and the known-bad control), and a seed vocabulary
  with a written contract per id, all off until tested. No I/O, no third-party dependencies.
- CI runs `--workspace`; the licence-gate floor is 18.

### Changed
- The `persistant` crate is unchanged in API and behaviour; its version now comes from
  `[workspace.package]`.

## 0.4.12

### Changed
- `docs/DESIGN-KINDS.md`, docs only: D1 recorded as ruled (C+, board 181, CireSnave 2026-10-14); adds the
  `kv.prefix_list` and `kv.prefix_list.exactly_once` leaves agreed with the auth-framework lane. No code
  or behaviour change.

## 0.4.11

### Changed
- Add `.github/CODEOWNERS` (`* @ciresnave`), the portfolio owner rule WSP-001. No code or behaviour change.

## 0.4.10

### Changed
- `docs/DESIGN-KINDS.md` revised, docs only, no code: a capability is a boolean that is off until an
  automated test passes (no declared/emulated/conditional status); the vocabulary is named by
  feature-space coverage rather than consumer demand; the matrix row schema (with `config`) and the
  trait / adapter / driver crate plan (`persistant-core`, `-blob`, `-relational`, `-kv`, `-opendal`,
  `-sqlx`) are written down; open decisions D2, D3 and D6 reworded.
- README: states the versions crates.io lists (0.4.1, 0.4.4, 0.4.8, 0.4.9 at 2026-10-13).

## 0.4.9

### Added
- `Error::Reentrant` (the enum is `#[non_exhaustive]`, so this is additive): a `blocking::Store` call
  made from a thread that is a blocking worker is refused instead of waiting on its own reply.
  Before, such a call parked the worker's only thread and deadlocked. No public API of this crate runs
  caller code on a worker, so this guards an invariant; it was found by audit, not reported in use.
  Also: dropping a worker from its own thread no longer tries to join itself.

## 0.4.8

### Changed
- `docs/DESIGN-KINDS.md`: design for PersistAnt as the one persistence layer for every kind, relational
  included (consumer inventory, one capability vocabulary, crate hierarchy, relational API options,
  adoption order, non-goals). Not shipped in the package; no code changes.
- README: the "no SQL" scope line is now "no SQL yet" with a pointer to the design, and states which
  versions crates.io holds (0.4.1 and 0.4.4 at 2026-10-11).

## 0.4.7

### Changed
- Docs only: the README says that `fs` cannot give atomic replace and exclusive create together.

## 0.4.6

### Added
- `Store::stat(key) -> Entry { key, len, modified }`; an absent key or a directory is `Error::NotFound`.
  Needs `Need::Stat`. `modified` is `Option`: `None` is unknown.
- `Store::list(prefix, after, limit) -> Page { entries, next }`, sorted by key, at most `limit` entries
  (`NonZeroUsize`), `next` is the cursor to pass as `after`. Needs `Need::List` and `Need::Stat`.
  **Emulated**: the prefix is a string prefix; the directory part is scanned and filtered, so memory is
  bounded by `limit` but each page rescans the tree.
- `Store::create_if_absent(key, bytes)`; `Error::AlreadyExists(key)` when the key has a value. Needs
  `Need::Write` and `Need::CreateIfAbsent`. Native, never emulated: `fs` with `atomic_write_dir` set is
  refused at open (OpenDAL checks then renames there, so it is not exclusive).
- The same three operations on `blocking::Store`; `Entry` and `Page` exported.
- Dependency: `futures-util` (already in the tree through OpenDAL), to read OpenDAL's lister.

## 0.4.5

### Changed
- CI only: every third-party GitHub Action is pinned to a full commit SHA read from upstream
  (the ref is kept as a comment). No crate change.

## 0.4.4

### Added
- `Error::NotFound(key)`: `Store::read` (and the blocking facade) of a key with no value now returns
  it instead of `Error::Backend(String)`, so a caller can tell an absent key from a disk error
  without matching message text. `Error` is `#[non_exhaustive]`, so this is additive, but code that
  matched `Backend` for a missing key must now match `NotFound` (closes #7).

## 0.4.3

Fixes from the independent retrospective review of PRs 2 and 3.

### Fixed
- `Needs::with` no longer records a need twice, so a refusal lists each missing need once.
- The fault-injecting fake now also faults `begin_replace` (previously only `replace`); an
  interrupted piece-wise replace half-writes each piece and fails at `commit`.
- `Faults::interrupt_writes_after` writes half rounded up, so a one-byte value is still a partial
  write; if the partial write itself fails the error is `Error::Injected`, not `Error::Backend`.
- An upgraded record that does not fit the current type is `Error::UpgradeFailed`, not `Corrupt`.

### Added
- `Error::UpgradeFailed` (`Error` is `#[non_exhaustive]`, so this is additive).
- Test: an interrupted replace leaves the old value on `memory`, which `AtomicReplace` acceptance relies on.

### Changed
- Docs: what `Faults` counts (attempts, not successes), that nested `Config::Faulty` keeps only the outermost.

## 0.4.2

### Fixed
- `blocking::Store`: an operation that panicked was reported as `Error::Backend("worker failed before
  replying")` with the panic lost. It is now `Error::Panicked(message)` and the store keeps working.

### Added
- `Error::Panicked` (`Error` is `#[non_exhaustive]`, so this is additive).

### Changed
- Crate description now mentions the blocking facade (a published description changes only with a new
  version). Docs: dropping a `blocking::Store` waits without a timeout for in-flight operations;
  the README cost numbers name the commit they were taken at.

## 0.4.1

### Changed
- Packaging only, no code change: an explicit `include` list so the published crate holds the source,
  tests, example, licences, README, CHANGELOG and SECURITY.md; `SECURITY.md` added (GitHub private
  vulnerability reporting).

## 0.4.0 (internal, never published)

### Added
- `persistant::blocking::Store`: the same operations for synchronous callers, on a private worker
  thread (safe inside any tokio runtime, from many threads). Tests include calls from inside current-thread
  and multi-thread runtimes. OpenDAL's own blocking operator was evaluated and rejected (see rustdoc).
- `examples/blocking_cost.rs`: per-call cost against `std::fs`.

## 0.3.0 (internal, never published)

### Added
- Typed records: `Record` trait (`KIND`, `SCHEMA`, `upgrade`), `Store::put_record` / `get_record`
  (JSON envelope `{kind, schema, data}`; newer schema, other kind and corrupt bytes are typed errors;
  older schema goes through `upgrade`), `Store::delete`.
- Fault-injecting fake: `Config::Faulty` with `Faults::{none, fail_writes_after, interrupt_writes_after}`.
  Wrappers (`Faulty`, `Masked`) nest.
- README usage example, compiled as a doctest.
- New errors: `Corrupt`, `Encode`, `KindMismatch`, `SchemaUnsupported`, `Injected`.

## 0.2.0 (internal, never published)

### Added
- `Store::open(config, needs)`: declare the guarantees a program needs (`Need`: read, write, delete,
  stat, list, create-if-absent, atomic replace, max value size); a backend that cannot give them is
  refused with `Error::Refused`, naming every unmet need. No silent downgrade.
- Atomic replace (`Store::replace`, `Store::begin_replace`) on the `fs` and `memory` backends. `fs`
  is accepted for `AtomicReplace` only when `atomic_write_dir` is configured (OpenDAL writes straight
  to the target otherwise); using replace without declaring it is `Error::NotDeclared`.
- `Config::Masked`: a capability-masking test fake.
- Depends on `opendal` 0.59 (`services-fs` only, default features off).

## 0.1.0 (internal, never published)

### Added
- Repository skeleton, CI (fmt, clippy, tests on ubuntu and windows; MSRV; cargo-deny; licence-header
  check), `MIT OR Apache-2.0` licence files. No storage API yet.
