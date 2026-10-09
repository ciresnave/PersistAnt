# Changelog

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
