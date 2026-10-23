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

A backend PersistAnt gains later reaches the program when it upgrades `persistant` and rebuilds; there are no plug-ins.

## What each `Need` becomes

Measured against `origin/main` at 0.9.28 (`matrix/*.tsv`, `src/registry.rs`).

| `Need` | Capability id | In the registry |
|---|---|---|
| `Read`, `Write`, `Delete` | `kv.get`, `kv.put`, `kv.delete` | on every listed backend |
| `List` | `kv.prefix_list` (or `kv.ordered_scan` if you need the order) | on every listed backend |
| `AtomicReplace` | none: not a separate id | every listed backend replaces a value whole. `fs` is the registry's `atomic_write_dir=set; put_mode=atomic` configuration (writes through a scratch directory beside the target, the one the blob-level crash test certifies, `blob.replace_atomic` in `matrix/opendal.tsv`); the SQL backends write one row in one statement |
| `CreateIfAbsent` | `kv.put_if_absent` | **not on `fs`**: with a scratch directory the exclusive create is check-then-rename, so the matrix has it off. Asking for it lists `mysql`, `postgres`, `sqlite` and `sqlite-full` at `Process` durability (`mariadb` and `memory` join at `Volatile`; run on 0.9.28) |
| `Stat`, `MaxValueSize` | none | there is no key-value counterpart: values are whole `Vec<u8>`; if you need a size limit, check it yourself |

And three things the old `Store` also gave that the registry **does not serve today** (the handle is the key-value kind only):

* **Append** (`blob.append`, certified on `opendal-fs`): agentlife's journal. Keep the file, or write one key per entry and read
  them back with `kv.ordered_scan`.
* **Locks** (`lock.process`, `lock.lease`): not offered by the registry. `lock.process` has its own row from 0.9.27 and
  is used directly through `persistant_opendal::ProcessLock`.
* **A synchronous call**: `registry::open_named` is `async`, and so is every `KvBackend` call. The older `persistant::blocking::Store`
  exists for synchronous programs; there is no blocking registry. agentlife measured about 11 ms per write on that facade
  (each write starts a worker thread and a runtime), so a synchronous program should hold one runtime and `block_on` it, not
  open per call.

## Durability

`Durability::Process` (survives the process exiting or crashing) is what the example asks for, and it excludes `memory`
(`Volatile`). A program whose data must survive a power cut asks for `Durability::Machine`, which lists `mysql`, `postgres` and
`sqlite-full` (run on 0.9.28) with their recorded basis (`Listing::why()` prints it); `fs` is not listed there because no fsync is
asserted for it.

## What to do with a configuration that already names a backend

Keep the old value working for one release: read the stored id if there is one; if there is none, map the old `Config`
to the nearest id (`Config::Fs` to `fs`), say so in the log, and write the id back. Do not guess at the endpoint: a
directory that held `Store` data holds `fs` data in the layout `KvOnBlob` uses (`kv/<key>` with a small envelope), not the
files `Store` wrote, so **existing data is not read by the new backend**. A migration is a one-time copy by the program.

## Why a program should do this at all

Today the three programs above cannot be given a SQLite or PostgreSQL store by their operators; the code says `Fs`.
With the registry they can, and the day PersistAnt gains a backend whose capabilities cover their needs, it appears
in their list without a code change.
