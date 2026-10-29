# Design note: the registry serves blob and relational, not only key-value

Status: **proposal, no code** (PM [TASK] 2026-10-28: design note first, code after it is merged). Spec-to-roadmap audit row 11.
Read at `a9ea1ee` (0.11.10): `src/registry.rs`, `crates/persistant-core/src/provider.rs`, `matrix/*.tsv`, `docs/REGISTRY-FOR-STORE-USERS.md`.

## 1. What is true today

- `registry::providers(&needs)` lists, best first, the providers compiled into the build that have every needed capability; `open_named(id, endpoint, needs)` gates twice and returns a `Handle` whose only accessor is `kv() -> &AnyKv`. The module doc says so: "a need for any other id is reported unmet rather than promised and not delivered."
- A `Provider` already has **one `Surface` per layer** (`provider.rs`: "a provider that is a kind built on another has one surface per layer, because the matrix files name them separately"). The registry fills only one surface, the kv one (`surface_for` returns `kv-on-blob:opendal-fs`, `kv-on-relational:sqlite`, ...). The gate says a capability is on when the matrix says `On` on **any** surface and `Off` on none.
- The matrix already has the other layers' rows, under their own backend names (counted at `a9ea1ee`): `opendal-fs` 20 blob rows, `opendal-memory` 9, `sqlite` 17 rel rows, `postgres` 17, `mysql` 17, `mariadb` 17. The id spaces do not overlap (`kv.*`, `blob.*`, `rel.*`, `lock.*`), so one need list can span kinds without ambiguity.
- The kinds exist as traits (`BlobBackend`, `RelationalBackend`, `RelationalTx`) and the drivers (`OpendalBlob`, `SqliteBackend`, `PgBackend`, `MysqlBackend`, with `Bounded`) are already linked by the registry features. What is missing is the enum a program calls through, and the gate that lists their surfaces.

So the work is plumbing, not new verification: no capability is added, no leaf changes.

## 2. Who asks

`docs/CONSUMERS.md` adoption order: auth-framework, commy, agentlife, lightbulb, synapse, Humboldt. Humboldt keeps its compile-time-checked SQL on the sqlx pool (board 230); it needs relational migrations and sweeps, which `persistant-migrate` and `persistant_relational::sweep` already give without the registry. The Store users (commy, agentlife, lightbulb) are blob-shaped: `REGISTRY-FOR-STORE-USERS.md` today tells them what does not map (`CreateIfAbsent` on `fs`, append, locks, "a blocking call"; `Stat` and `MaxValueSize` "have no key-value counterpart"). A blob handle is the honest answer for `Stat`, `append`, `replace_atomic` and listing. **No consumer has asked in writing for the relational handle**; the PM ruled that rows are picked without waiting for one ([TASK] 2026-10-28), so this note sizes it and lets the PM cut it.

## 3. Design

### 3.1 Providers list every kind they can serve

Each `Spec` gains the other layers' backend names:

| id | kv surface (today) | blob surface (new) | rel surface (new) |
|---|---|---|---|
| `memory` | `kv-on-blob:opendal-memory` | `opendal-memory` | none |
| `fs` | `kv-on-blob:opendal-fs` | `opendal-fs` (same `atomic_write_dir` fact) | none |
| `sqlite` | `kv-on-relational:sqlite` | none | `sqlite` |
| `sqlite-full` | `kv-on-relational:sqlite-full` | none | see 5.1 |
| `postgres` | `kv-on-relational:postgres` | none | `postgres` |
| `mysql`, `mariadb` | `kv-on-relational:*` | none | `mysql`, `mariadb` |

A need for `blob.stat` on `sqlite` finds no surface that has it: `Unmet`, naming `blob.stat` on `sqlite` ("no row" is off). **No closest match** is kept; nothing is emulated to make a SQL database look like a blob store (that is also a patent condition, `PATENT-SWEEP.md`: no emulation of an unsupported operation from stored per-backend metadata).

The primary surface stays the kv one, because `rank` matches speed rows on the primary. A need list with no `kv.*` id would then rank on kv speed rows it did not ask for; fix: the primary is the first surface whose kind the needs name (kv, then blob, then rel). Ranking across kinds is not offered: a need list naming two kinds is ranked on the first kind's workloads only, and `Listing::why` says so.

### 3.2 The handle

```text
Handle::kv()   -> &AnyKv                 // unchanged
Handle::blob() -> Option<&AnyBlob>       // Some iff the needs named a blob.* id
Handle::rel()  -> Option<&AnyRel>        // Some iff the needs named a rel.* id
```

`AnyBlob` wraps `OpendalBlob` and implements `BlobBackend` (forwarding all nine methods and `key_layout`). `AnyRel` is the enum of the three drivers, each plain and `Bounded`, implements `RelationalBackend` with `type Tx<'a> = AnyTx<'a>` (an enum of the driver transactions forwarding `RelationalTx`). Both mirror `AnyKv`, with the `each!` macro.

**Open question for the PM:** whether `kv()` keeps its type.

- **A (recommended): additive.** `kv()` keeps its type, so the kv store is always opened. The cost, stated: a program that wanted only `rel.*` on `sqlite` also gets the `persistant_kv` table created in its file (the open path creates tables first), and a blob-only program on `fs` gets a `KvOnBlob` over its directory (it writes under `kvx/` only when used). Harmless, but a side effect the program did not ask for.
- **B: breaking (0.12).** `kv()` returns `Option<&AnyKv>` like the others; nothing is created for a kind not named. Cleaner, costs every `registry` caller one `.expect`/`?`.

Recommendation: ship A now (0.11.x, additive), re-decide when the first rel-only consumer reports the stray table. B is a one-line signature change plus a CHANGELOG `Breaking`.

### 3.3 Shared endpoint, shared namespace

One provider has one endpoint, so one `fs` directory or one SQLite file serves several kinds. The namespaces must not collide:

- `fs`: `KvOnBlob` writes under `kvx/` (hex layout, 0.9.42) and, for a legacy directory, `kv/` until migrated. **The blob handle of `fs` reserves `kv/` and `kvx/`**: `AnyBlob` refuses keys under them (`BlobError::Backend` naming the reserved prefix, tested). Cost: a program that stores blob keys beginning `kv/` or `kvx/` cannot use the registry's `fs` for blobs; stated in the guide.
- SQL: the relational handle shares the database with the kv table. `KvOnRelational`'s table name is reserved; `AnyRel` does not hide it (a raw `DROP TABLE` can still break kv, as with any shared database), but the guide says to give a rel-only program its own database file, and the kv table is not created under B.

### 3.4 The gate, twice

Unchanged in shape: first against the table's recorded surfaces, then against what was actually opened. The second check now builds one `Surface` per opened kind from the real `backend()` / `version()` / config of that object, so a driver that moved off the recorded rows is refused for every kind served, with the unmet ids. The `call_bound` fact is attached to the kv and rel surfaces (the blob driver has no bound; `BoundUnsupported` stays for `memory` and `fs`).

### 3.5 Durability

`ProviderNeeds` carries one minimum class. With several kinds served, the provider's effective class is the **weakest** declared class among the surfaces the needs name (kv, blob, rel each have `declared_durability`); `Listing::why` prints which surface set the class. The blob kind's class is declared and unverified, as today.

### 3.6 Also mirrored

`src/blocking/registry.rs` mirrors the async registry for blocking callers; it gets `blob()` / `rel()` in the same work order as the async one.

## 4. Work orders (each a PR; none starts before this note merges)

1. **Surfaces and gate.** `Spec` carries blob and rel backend names; `provider_for` builds all surfaces; the primary-surface rule; `Listing::why`; the weakest-class rule; second-gate surfaces. No `Handle` change yet. Tests: a blob need on `fs` lists `fs` and `memory` and not `sqlite`; a rel need lists `sqlite` and not `fs`; a mixed need (`kv.get` + `blob.stat`) lists `fs` only; a need that no provider covers is `Unmet` and names each backend's missing id (the known-bad control: ask `sqlite` for `blob.stat`).
2. **`AnyBlob` and `Handle::blob()`**, with the reserved-prefix guard. Tests: the existing blob contract (`conformance::certify`) run through `AnyBlob` on `memory` and `fs`, and a key under `kvx/` is refused.
3. **`AnyRel`, `AnyTx` and `Handle::rel()`.** Tests: `rel.transaction`, `rel.error.unique` and `rel.migrate.apply` through `AnyRel` on SQLite; PostgreSQL and MySQL on the CI service containers as for `registry_servers.rs`.
4. **Blocking mirror, the guide (`REGISTRY-FOR-STORE-USERS.md`: `Stat`, append, replace and listing now map to `blob()`), `examples/registry_blob.rs`.**

Each bumps the version; 1 and 4 are additive, 2 and 3 are additive under A.

## 5. Open items found while reading

1. **`sqlite-full` has no relational rows.** *(Resolved in 0.12.1: the matrix job certifies the relational kind and the migration ledger on the durable profile, measured not copied; `declared_durability` already named it `Machine`.)* `SqliteBackend::open_durable` reports `backend() == "sqlite-full"` (`crates/persistant-sqlx/src/sqlite.rs:263`) but `matrix/sqlx-sqlite.tsv` records the 17 rel rows only under `sqlite`. A rel need on provider `sqlite-full` would therefore be unmet ("no row"), which is wrong for a backend that is the same engine with a stricter flush. Work order 1 must either have the matrix job also certify `sqlite-full` for the rel kind (preferred: it is measured, not copied) or the registry must refuse rel on `sqlite-full` until it does. Not decided here; it needs the matrix job, so a CI run.
2. **`blob.max_value_size`** (audit row 2) and **`blob.file_mode`** (row 3) become visible to registry users the moment `blob()` exists; both are untested and off, so a need for them is unmet today, which is the right answer until a test exists.
3. **Declared durability for `rel`:** `persistant_relational::choose::declared_durability` exists; whether it names `sqlite-full` as `Machine` as the kv one does is to be read in work order 1.

## 6. Not in this note

Redis (`CONSUMERS.md` row 6, not a PersistAnt backend), streamed blob I/O (row 4, mlmf), `lock.*` through the registry (still not offered), a dynamic plugin mechanism (the table stays compiled in, ids stay append-only), and ranking across kinds.

## 7. Decisions asked of the PM

1. Cut the work: all four orders, or 1 and 2 (blob only, which is what the Store users need) with 3 held until a consumer asks for rel?
2. A or B for `kv()` (recommend A).
3. Approve the reserved `kv/` and `kvx/` prefixes for the registry's `fs` blob handle.
