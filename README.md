# PersistAnt

A **thin** persistence layer over [Apache OpenDAL](https://opendal.apache.org/), for **blobs and records
stored by key**. Programs declare the storage guarantees they need; PersistAnt refuses a backend that cannot
give them, instead of silently doing something weaker.

> Status: 0.2.0 has capability declaration with refusal, and atomic replace on the `fs` and `memory`
> backends. Records, migrations, locks, expiry and the fault-injecting fake are still to come.

## What it will own

1. **Capability declaration with refusal at startup.** A program says what it needs (atomic replace,
   create-if-absent, list by prefix, durable write). Opening a store on a backend that cannot provide it
   returns a typed error naming what is missing. No silent downgrade.
2. **An atomic-replace helper** (write a temporary file, flush, rename). OpenDAL's `fs` service writes
   atomically only when `atomic_write_dir` is configured; PersistAnt configures and tests it.
3. **Typed records with schema versions, migrations for non-SQL stores, lock and expiry helpers, and test
   fakes** (in-memory and fault-injecting). OpenDAL has no TTL and no locks, so those are emulated; the
   documentation will say what is emulated and where the limits are.
4. **One place that absorbs OpenDAL's breaking changes.** OpenDAL types stay out of the public API unless
   unavoidable.

## What it does not do

- **No SQL, queries, joins or secondary indexes.** Relational code stays on sqlx and the database. OpenDAL's
  `postgresql`, `sqlite` and `redis` services are key/value tables, not SQL.
- **No multi-key transactions.** OpenDAL has none, so PersistAnt does not pretend to.
- **No OS file locks, directory trees handed to other tools, or memory-mapped model files.**

## Limits

- **Maximum value size cannot be refused on `memory` or `fs`.** OpenDAL reports `write_total_max_size: None`
  for them, meaning unlimited, not unknown. `Need::MaxValueSize(n)` is refused only by a backend that
  advertises a smaller ceiling (for example Cloudflare D1, 1 MB).
- **`atomic_write_dir` must be on the same filesystem as the root and outside it.** PersistAnt checks that it
  is set, not where it is.
- **`memory` counts as atomic** because a value becomes visible in one map insert on close.

## Licence

Licensed under either of [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE) at your option.
