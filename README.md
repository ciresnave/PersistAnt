# PersistAnt

A **thin** persistence layer over [Apache OpenDAL](https://opendal.apache.org/), for **blobs and records
stored by key**. Programs declare the storage guarantees they need; PersistAnt refuses a backend that cannot
give them, instead of silently doing something weaker.

> Status: 0.1.0 is the repository skeleton and CI only. There is no storage API yet.

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

## Licence

Licensed under either of [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE) at your option.
