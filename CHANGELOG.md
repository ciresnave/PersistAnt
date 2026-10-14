# Changelog

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
