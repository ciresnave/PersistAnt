# Moving from `Store` + `Config` to the registry

For a program that declares `Needs` and opens `Store::open(Config::Fs { .. }, needs)` (the persistant 0.4 API:
`commy`, `agentlife` and `lightbulb`'s disk tier per `docs/CONSUMERS.md`). That API checks your needs but **you name
the backend in code**. CireSnave's standing order (2026-10-21, `CIRESNAVE-EXPECTATIONS.md` 6.17) is that a project
states what it needs, PersistAnt lists the backends that qualify, the operator's choice is stored in the project's
configuration, and the program opens that. This is the mapping, and what does not map.

A runnable version of the steps is `examples/registry_instead_of_config.rs`
(`cargo run --example registry_instead_of_config --features registry`; add `registry-sqlite` and an id such as
`sqlite` to try another backend). The module documentation of `persistant::registry` is the reference.

## The four steps

1. **State needs as capability ids**, with the least durability class: `ProviderNeeds::new(ids, Durability::Process)`.
2. **Ask which backends qualify**: `registry::providers(&needs)` returns them best first, each with `why()` lines. Show that
   list wherever the operator configures the program.
3. **Store the choice in your own configuration**: the `Listing::id` (a stable string, append-only) and an `Endpoint`
   (`Endpoint::path(dir)` for files, `Endpoint::url_env("VAR")` for a server URL held as a secret).
4. **Open it at start-up**: `registry::open_named(id, &endpoint, &needs).await`. The gate is checked again there; a backend that no
   longer meets the needs is `OpenError::Unmet`, naming the ids. Then use `handle.kv()` and nothing backend-specific.

A backend PersistAnt gains later reaches the program when it upgrades `persistant` and rebuilds; there are no plug-ins. A provider
is in `providers()` only if **its Cargo feature is compiled in**: `registry` gives `memory` and `fs`, `registry-sqlite` adds `sqlite`
and `sqlite-full`, `registry-postgres` adds `postgres`, `registry-mysql` adds `mysql` and `mariadb`, `all-backends` is every one. Enable
the feature of each backend you want an operator to be able to pick; upgrading alone adds none.

## What each `Need` becomes

Measured against `origin/main` at 0.9.28 (the `CreateIfAbsent` row at 0.9.41) (`matrix/*.tsv`, `src/registry.rs`).

| `Need` | Capability id | In the registry |
|---|---|---|
| `Read`, `Write`, `Delete` | `kv.get`, `kv.put`, `kv.delete` | on every listed backend |
| `List` | `kv.prefix_list` returns **keys only, in no promised order**; `kv.ordered_scan` returns keys in ascending bytewise order | on every listed backend. `Store::list` gave ascending order *and* an `Entry` (`len`, `modified`): use `ordered_scan` where order mattered, and note that **neither returns a length or a modification time**: if you used them, store them in the value or under a second key |
| `AtomicReplace` | none: not a separate id | every listed backend replaces a value whole. `fs` is the registry's `atomic_write_dir=set; put_mode=atomic` configuration (writes through a scratch directory beside the target, the one the blob-level crash test certifies, `blob.replace_atomic` in `matrix/opendal.tsv`); the SQL backends write one row in one statement |
| `CreateIfAbsent` | `kv.put_if_absent` | on `fs` since 0.9.41: with a scratch directory the driver writes a finished scratch file and hard-links it to the key (OpenDAL's own create there is check-then-rename), so one of many creators wins and the value is whole when visible. It needs the scratch directory and the root on one filesystem with hard links. Asking for it lists `fs`, `mysql`, `postgres`, `sqlite` and `sqlite-full` at `Process` durability (`mariadb` and `memory` join at `Volatile`; run on 0.9.41). The old `Store::create_if_absent` still refuses `atomic_write_dir` |
| `Stat` | `blob.stat`, through `handle.blob()` | the blob handle (0.11.14): `stat` gives the size without reading the value. Ask for the `blob.*` ids you use and read `Handle::blob()`; `fs` and `memory` serve it (the key-value store has no counterpart) |
| `MaxValueSize` | none | `blob.max_value_size` is untested and off, so a need for it is unmet; check the size yourself |

And three things a program may use *beside* the `Store` API (the old `Store` has no append and no locks; these come from the blob driver and `persistant-opendal`); the registry serves the first, not the second, and the third is below:

* **Append** (`blob.append`, certified on `opendal-fs`): agentlife's journal. Served since 0.11.14 by `Handle::blob()` (or the
  blocking `KvHandle::blob()`): `append` adds whole entries in call order, and `replace_atomic` and `list` come with it.
  `examples/registry_blob.rs` runs all of it on `fs`. **Two limits:** the keys `kv`, `kvx` and anything under `kv/` or `kvx/`
  are reserved on every provider (the key-value store keeps its files there on `fs`), so a key such as `kv/journal` is refused;
  and `blob()` is `Some` only when the needs name a `blob.*` id.
* **Relational** (`rel.*`): `Handle::rel()` (0.11.15) gives statements and transactions on `sqlite`, `postgres`, `mysql` and
  `mariadb`, in the dialect it reports. The blocking `KvHandle::rel()` has `execute`, `query` and `transaction(isolation,
  statements)`, which runs a list of statements in one transaction and rolls all back if one fails.
* **Locks** (`lock.process`, `lock.lease`): not offered by the registry. `lock.process` has its own row from 0.9.27 and
  is used directly through `persistant_opendal::ProcessLock`.
* **A synchronous call** is served from 0.9.32 by `persistant::blocking::registry`: the same `providers`, and `open_named`
  returns a `KvHandle` with blocking `get`, `put`, `delete`, `put_if_absent`, `prefix_list`, `take`, `count` and the rest. It
  uses the worker of `blocking::Store` (one thread, a private single-threaded runtime), so it is safe from plain code, from many
  threads and from inside another runtime. **Keep one handle for the life of the program**: agentlife measured about 11 ms per
  write on the older facade because each write started a worker thread and a runtime, and opening per call would repeat that.

## Durability

`Durability::Process` (survives the process exiting or crashing) is what the example asks for, and it excludes `memory`
(`Volatile`). A program whose data must survive a power cut asks for `Durability::Machine`, which lists `mysql`, `postgres` and
`sqlite-full` (run on 0.9.28) with their recorded basis (`Listing::why()` prints it); `fs` is not listed there because no fsync is
asserted for it.

## Server backends: bound every call

A server can stall rather than fail. The registry says so for MySQL on a full disk: the call gets no answer and no error. A
program that opens `mysql`, `mariadb` or `postgres` with `open_named` and nothing else makes unbounded calls. Ask for
`kv.bounded_call` in the needs and pass a bound:

```rust
let options = OpenOptions::new().call_bound(std::time::Duration::from_secs(10));
let handle = registry::open_named_with(id, &endpoint, &needs, &options).await?;
```

A call that exceeds the bound returns `KvError::TimedOut` and a batch that timed out applied nothing; a single write that timed
out may or may not have taken effect. A bound is refused (`OpenError::BoundUnsupported`) for `memory` and `fs`, which have no
server to wait for. Without it, wrap each call in your own timeout.

## `fs` and keys that differ only by case

On a case-insensitive filesystem (NTFS and APFS by default) two keys that differ only by case would be **the same file** if a key were a
file path. Measured on Windows, 2026-10-23: `put("Key", "upper")` then `put("key", "lower")` then `get("Key")` returned `lower`. From
0.9.33 `open_named("fs", ..)` refused such a directory (`OpenError::CaseInsensitiveFilesystem`). **From 0.9.42 `fs` works there**: it
stores each key's bytes in lower-case hex (path segments of at most 100 characters, under `kvx/` in the directory), so no two keys share a
file whatever the filesystem does with case, separators, dots or reserved names (`CON`, `a:b`, `..`), a key such as `a` can sit beside
`a/b`, and bytewise order and prefixes are kept, so `ordered_scan` and `prefix_list` are unchanged. The price is a directory of hex names
that is not meant to be read by hand, and Windows' 260-character path limit applies to the whole path (root, `kvx/`, and two hex
characters per key byte). `OpenError::CaseInsensitiveFilesystem` is still defined so existing `match` arms compile; `open_named` no
longer returns it. A directory written by 0.9.41 or earlier (raw `kv/<key>` files) is **moved to the hex layout when it is opened**
(an exclusive copy, then a delete, so a newer value is never overwritten and repeating it is harmless); `KvOnBlob::migrate_raw_layout`
does the same for a program that uses the adapter itself.

## What to do with a configuration that already names a backend

Do not guess at the endpoint: a directory that held `Store` data holds `fs` data in the layout `KvOnBlob` uses (`kvx/<hex of the key>/-` with a
small envelope since 0.9.42; `kv/<key>` before), not the files `Store` wrote, so **existing data is not read by the new backend**. Moving is a one-time copy by the
program, and its order matters:

1. **Drain the old writers first.** A rolling upgrade, with old and new instances running together, loses writes: an old instance
   writes to the old layout after the copy, and new instances read only the new one. Stop every old instance (or make each write go
   to both stores until none is left), then copy.
2. **Copy, then verify, then switch.** Read every key from the old `Store`, `put` it through the registry, and compare the counts
   and the values (or a digest) before going on. The copy can be repeated: a `put` overwrites.
3. **Write the new id to your configuration last.** If the id is stored before the copy ends and the program stops, the next start
   opens the new layout and sees a store that is missing data. With the id written last, an interrupted migration simply runs again.
4. **Keep the old data** until the new store has been used for a while; delete it by a separate, deliberate step.

Between those, keep the old configuration value readable for one release: if no id is stored, map the old `Config` to the nearest id
(`Config::Fs` to `fs`), say so in the log, and do not write the id back until the copy has been verified.

## Why a program should do this at all

Today the three programs above cannot be given a SQLite or PostgreSQL store by their operators; the code says `Fs`.
With the registry they can, and the day PersistAnt gains a backend whose capabilities cover their needs, it appears
in their list without a code change.
