# PersistAnt

A **thin** persistence layer over [Apache OpenDAL](https://opendal.apache.org/), for **blobs and records
stored by key**. Programs declare the storage guarantees they need; PersistAnt refuses a backend that cannot
give them, instead of silently doing something weaker.

> Status: pre-1.0 (the API may change between minor versions). It has capability declaration with refusal, atomic replace on the `fs` and `memory`
> backends, typed records with schema versions, and in-memory, capability-masking and
> fault-injecting fakes, and a blocking facade (`persistant::blocking`) for synchronous code. Migrations,
> locks and expiry are still to come. The main API is async.

## Usage

```rust
use persistant::{Config, Need, Needs, Record, Store};
use serde::{Deserialize, Serialize};

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct Agent {
    name: String,
    restarts: u32,
}

impl Record for Agent {
    const KIND: &'static str = "agent";
    const SCHEMA: u32 = 1; // bump when the shape changes, and implement `upgrade`
}

# #[tokio::main(flavor = "current_thread")]
# async fn main() -> Result<(), persistant::Error> {
# let dir = tempfile::tempdir().unwrap();
# let scratch = tempfile::tempdir().unwrap();
// Say what the program needs. A backend that cannot give it is refused here, by name.
let needs = Needs::new()
    .with(Need::Read)
    .with(Need::Write)
    .with(Need::Delete)
    .with(Need::AtomicReplace);

// On `fs`, atomic replace requires a scratch directory (`atomic_write_dir`); without it, open fails.
let store = Store::open(
    Config::Fs {
        root: dir.path().to_path_buf(),
        atomic_write_dir: Some(scratch.path().to_path_buf()),
    },
    needs,
)
.await?;

let a = Agent { name: "scout".into(), restarts: 2 };
store.put_record("agents/scout", &a).await?;
assert_eq!(store.get_record::<Agent>("agents/scout").await?, Some(a));
store.delete("agents/scout").await?;
assert_eq!(store.get_record::<Agent>("agents/scout").await?, None);
# Ok(())
# }
```

## Synchronous callers

`persistant::blocking::Store` has the same operations (`open`, `read`, `replace`, `delete`, `put_record`,
`get_record`) without `async`. It runs the work on a worker thread of its own with a private runtime, so
it is safe to call from plain code, from inside another tokio runtime of either flavour, and from many
threads. (OpenDAL's own blocking operator needs to be created inside a running runtime and panics when
called from within one, so it is not used.) Inside an async task the call still blocks that task's thread:
use the async `Store` there. A call made from a worker thread of a blocking `Store` is refused with
`Error::Reentrant` rather than waiting on itself; no API of this crate runs your code on a worker, so
this guards an invariant and is not a path you can reach today.

```rust
use persistant::{blocking::Store, Config, Need, Needs};

let store = Store::open(Config::Memory, Needs::new().with(Need::Read).with(Need::Write).with(Need::AtomicReplace))?;
store.replace("greeting", b"hello".to_vec())?;
assert_eq!(store.read("greeting")?, b"hello");
# Ok::<(), persistant::Error>(())
```

**Cost.** Each call crosses to the worker and back: about 40 to 60 microseconds on the `memory` backend
(median, 1 byte to 4 KiB, one Windows 11 machine, taken on the code of commit `e734735`; the facade and the example are unchanged on `0ceb7b1`, the 0.4.1 release). On `fs` the larger cost is OpenDAL's file path: for a
4 KiB value a read took about 1.5 ms against 0.1 ms for `std::fs::read`, and an atomic replace about 5 ms
against 0.7 ms for a plain, non-atomic `std::fs::write` (the sync before the rename). For multi-MiB values the
ratio falls to roughly 1.2 to 1.7 times `std::fs`. `cargo run --release --example blocking_cost` measures it on
your machine; measure before putting it on a hot path.

## Testing your own failure handling

`Config::Memory` is a fake store. `Config::Faulty` makes writes fail (or be interrupted half-way) after
a count you choose, and `Config::Masked` makes a backend report that it lacks some needs, so a test can
show that a program refuses a weaker production backend. Wrappers nest.

## Scope

**Shipped:**

1. **Capability declaration with refusal at open.** A program says what it needs (`Need`: read, write,
   delete, stat, list, create-if-absent, atomic replace, maximum value size). Opening a store on a backend
   that cannot provide every need returns a typed error naming all that are missing. No silent downgrade.
   Operations: read, write, delete, atomic replace, `stat`, `list` and `create_if_absent`. `list` is
   paged and sorted (`limit`, `after` cursor) and **emulated**: OpenDAL lists directories, so a string
   prefix is a directory scan plus a filter (memory bounded by `limit`, time not). `create_if_absent`
   is native or refused: on `fs` it is refused when `atomic_write_dir` is set, because OpenDAL's
   atomic path checks then renames and two creators can both pass. So on `fs` a program cannot have
   both atomic replace and exclusive create from OpenDAL 0.59.4: declaring both is refused at open.
2. **Atomic replace.** OpenDAL's `fs` service writes atomically only when `atomic_write_dir` is configured;
   PersistAnt refuses `fs` without it when atomic replace is declared, and tests the real service.
3. **Typed records with schema versions**, and **test fakes**: in-memory, capability-masking and
   fault-injecting.
4. **A blocking facade** for synchronous code.
5. **OpenDAL types stay out of the public API**, so OpenDAL's breaking changes land in this crate.

**Not yet:** migrations for non-SQL stores, lock helpers and expiry helpers. OpenDAL has no TTL and no
locks, so those will be emulated, and the documentation will say what is emulated and where the limits are.

## Direction

**This crate is the blob and record-by-key part of a wider layer.** On 2026-10-11 the scope was widened:
PersistAnt is to be the one persistence layer for every kind of persistence, relational databases
included, organised as one capability vocabulary with a crate per kind. The design, with the evidence
behind it and the decisions still open, is
[`docs/DESIGN-KINDS.md`](https://github.com/ciresnave/PersistAnt/blob/main/docs/DESIGN-KINDS.md).
Nothing in it is implemented yet; the list under "What it does not do" describes **this crate today**.

## What it does not do

- **No SQL, queries, joins or secondary indexes, yet.** This crate is blob and record-by-key only.
  OpenDAL's `postgresql`, `sqlite` and `redis` services are key/value tables, not SQL. Relational support
  is designed in `docs/DESIGN-KINDS.md` and is not built.
- **No multi-key transactions.** OpenDAL has none, so PersistAnt does not pretend to.
- **No OS file locks, directory trees handed to other tools, or memory-mapped model files.**

## Limits

- **Maximum value size cannot be refused on `memory` or `fs`.** OpenDAL reports `write_total_max_size: None`
  for them, meaning unlimited, not unknown. `Need::MaxValueSize(n)` is refused only by a backend that
  advertises a smaller ceiling (for example Cloudflare D1, 1 MB).
- **`atomic_write_dir` must be on the same filesystem as the root and outside it.** PersistAnt checks that it
  is set, not where it is.
- **An abandoned replace leaves its temporary file** in `atomic_write_dir` on `fs`; the old value is intact
  but the scratch directory should be swept at startup. **Durability across power loss is not claimed**:
  OpenDAL syncs the file, not the parent directory, before the rename.
- **`memory` counts as atomic** because a value becomes visible in one map insert on close.

## The other kinds (opt-in features)

`persistant` is one name for several crates (design: `docs/DESIGN-KINDS.md`). Everything above is
unchanged; these features, all off by default, re-export the rest under one module each:

- `core` as `persistant::capability`: capability ids, the checked-in matrix file and the gate lookup.
- `blob` as `persistant::blob`: the `BlobBackend` trait, a memory reference backend, contract tests.
- `kv` as `persistant::kv`: the `KvBackend` trait, key-value over blob or over a relational
  database, leases, contract tests.
- `relational` as `persistant::relational`: a deliberately small database trait and typed errors.
- `opendal-driver` as `persistant::opendal_driver`: the blob driver and `ProcessLock`.
- `sqlx-driver` as `persistant::sqlx_driver`: PostgreSQL over sqlx. **Needs Rust 1.94**, as does every
  crate here since 0.4.36 (UniSQL 0.24 is 1.94).

Nothing here is on by derivation: a capability is on for a backend and version only where a
recorded test says so (`matrix/*.tsv`).

## Published versions

crates.io lists 0.4.1, 0.4.4, 0.4.8 and 0.4.9 (API read 2026-10-13, none yanked); 0.4.5 to 0.4.7 are
not published. Check the registry, not this line, before depending on one.

## Licence

Licensed under either of [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE) at your option.
