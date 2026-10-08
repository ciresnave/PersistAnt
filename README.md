# PersistAnt

A **thin** persistence layer over [Apache OpenDAL](https://opendal.apache.org/), for **blobs and records
stored by key**. Programs declare the storage guarantees they need; PersistAnt refuses a backend that cannot
give them, instead of silently doing something weaker.

> Status: 0.3.0 has capability declaration with refusal, atomic replace on the `fs` and `memory`
> backends, typed records with schema versions, and in-memory, capability-masking and
> fault-injecting fakes. Migrations, locks and expiry are still to come. The API is async.

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

## Testing your own failure handling

`Config::Memory` is a fake store. `Config::Faulty` makes writes fail (or be interrupted half-way) after
a count you choose, and `Config::Masked` makes a backend report that it lacks some needs, so a test can
show that a program refuses a weaker production backend. Wrappers nest.

## What it owns, and what is still to come

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
- **An abandoned replace leaves its temporary file** in `atomic_write_dir` on `fs`; the old value is intact
  but the scratch directory should be swept at startup. **Durability across power loss is not claimed**:
  OpenDAL syncs the file, not the parent directory, before the rename.
- **`memory` counts as atomic** because a value becomes visible in one map insert on close.

## Licence

Licensed under either of [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE) at your option.
