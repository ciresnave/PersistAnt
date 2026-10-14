# Changelog

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
