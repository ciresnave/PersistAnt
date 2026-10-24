# Who stores what: a survey of the portfolio's storage, and what PersistAnt would have to give

Status: **survey, 2026-10-23, read-only, no code anywhere.** PM [TASK] 2026-10-23, from CireSnave's standing order.
The order, verbatim (2026-10-21, `CIRESNAVE-EXPECTATIONS.md` 6.17):

> *"Synapse should support any storage backend that PersistAnt can use which provides the necessary capabilities Synapse needs.  The same hold true for all of our projects.  I don't want to arbitrarily limit any project to a specific backend when there are multiple options available.  That was the whole point of using PersistAnt rather than directly integrating a storage backend."*

So the question per project is: does it **declare** what it needs, **pick** a backend through PersistAnt (`registry::providers`,
`open_named`, `choose`) instead of naming one, and what stops it.

## 1. Method, and what it cannot show

* Twelve repositories, each `git fetch`ed, read at its own `origin/main` (SHAs in the table); never a working tree
  (`CLAUDE.md` section 2). Eleven are Rust; `ThinkersJournal.com` is an Astro site, read through `package.json`.
* **Storage dependencies**: `git grep -E` on every tracked `Cargo.toml` for `rusqlite|sqlx|redb|redis|sled|rocksdb|tokio-postgres|
  postgres|deadpool|diesel|sea-orm|heed|lmdb|fjall|persistant*|opendal|mysql*|libsqlite3-sys|surrealdb|r2d2|bb8|mongodb|
  memmap2|dashmap|moka|cached|bincode|rkyv`.
* **File persistence by hand**: `git grep -c` on `*.rs` for `fs::write|File::create|OpenOptions|fs::rename|serde_json::to_*|.jsonl`.
* **Declared needs**: the project's own design documents, where it has one (named per row). Where a project has none, the
  row says "not declared" and gives what the code does instead; that is an observation, not a need.
* **Positive controls** (the same grep must find what is known to be there):
  * Dependency grep on PersistAnt's own tree at `1e62eaa8` finds `sqlx`, `opendal` and every `persistant-*` crate (41 lines).
  * The same kind of grep for `serde` finds it in `Cargo.toml` of 9 of the 12 (the others are `commy-sdk-rust`, `KISS` and
    `ThinkersJournal.com`, which has no `Cargo.toml`), so an empty storage result for a repo that has `serde` is a real
    empty result. Zero storage hits is a statement about `Cargo.toml` only.
  * The one place the first scan implied something false: it found no append primitive in agentlife's use of PersistAnt,
    and `blob.append` and `blob.append.concurrent_whole` are in the vocabulary **and certified on `opendal-fs`**
    (`matrix/opendal.tsv`); the gap was in the reading, not in PersistAnt.
* **Limits.** A manifest scan does not see a dependency pulled in by a path or a git source, a store written by
  a subprocess, or a file format hidden behind a trait. Sizes, concurrency and encryption needs are listed only where a
  project's code or document states them; otherwise "not stated". Nothing was run or benchmarked.

## 2. The three levels of adoption

1. **Registry** (`persistant::registry`, `providers` / `open_named`): the program states capability ids and a durability
   class, PersistAnt lists the backends that meet them, the operator's choice is stored in configuration. Only Synapse.
2. **`persistant::Store` with a `Config`** (the pre-registry API, `Needs` such as `Read`, `Write`, `AtomicReplace`): the
   needs are declared and checked at open, but the **backend is named in code** (`Config::Fs { .. }`). commy, agentlife,
   lightbulb's disk tier.
3. **A backend named directly** (`sqlx::Pool`, `redb`, `redis`, hand-written files): Humboldt, auth-framework, the per-tenant
   databases in commy, Synapse's default fall-back, and every file a project writes by hand.

## 3. The projects

| Project @ `origin/main` | Stores | Backend now (level) | Declared needs | PersistAnt capability it maps to | Blockers |
|---|---|---|---|---|---|
| **synapse** `cf6cf6a0` | the mailbox (queues, leases, acks); other stores (keystore, session cache, security log) **not read** | PersistAnt registry through `synapse-mailbox-persistant` (`persistant 0.9.10`, sqlite / postgres / mysql / mariadb); `redb 4.4` behind feature `mailbox-redb` (CireSnave: "Redb is fine for now"); on a fresh home the first listed *file* store, else redb; env `SYNAPSE_MAILBOX_STORE` deprecated. **Level 1** | `docs/MAILBOX_STORE_NEEDS.md`: durability class at least Machine, atomic multi-key write, ordered scan, lease / visibility timeout, ttl and retention, idempotence, bounds without loss, roles written through, a stall becomes an error | `kv.get put delete put_if_absent compare_and_swap compare_and_delete prefix_list` and the batch leaves, built into `provider_needs` (`needs.rs`); durability from `open_durable` (sqlite-full) | its own listed gaps: performance unmeasured (gap 2), no in-place path off redb (gap 5); the redb fall-back means a fresh home may still not use PersistAnt |
| **commy** `de020650` | service registry (`services/<id>`, replaced atomically) and grants (append-only log `grants/<seq>`), audit log (ROADMAP R5, R6); per-tenant auth databases `<tenant>.sqlite` | the registry, grants and audit on `persistant 0.8.21` (`persist.rs`, `persistant::blocking::Store`; **level 2**, opened on a directory); tenant auth through `auth-framework` `SqliteStorage` and `sqlx` directly (`framework_verifier.rs`, **level 3**, files created private); `memmap2` for the shared-memory data plane (not persistence) | `persist.rs` header: write-through and fail closed, nothing stored for sessions or nonces; per-tenant isolation | `blob.replace_atomic`, `blob.create_exclusive`, `blob.list` (grants by sequence); the auth databases map to auth-framework's needs below | pin `0.8.21` is behind (current 0.9.24); the tenant databases wait on auth-framework |
| **commy-sdk-rust** `8838377a` | nothing durable; maps a service file the server names and reads a 0600 nonce side file | none in `Cargo.toml` | not applicable | none | none; **not a candidate** |
| **agentlife** `5af2206b` | the append-only journal (`journal-YYYY-MM.jsonl`, own `FileLock`), registry files, restore reports, `lane-restart` state files and `restart.log` | `persistant 0.4.4` for atomic file replace only (`atomic.rs`: `Need::Write` + `AtomicReplace`, `Config::Fs`, **level 2**); the journal and the `lane-restart` logs are hand-written file appends (**level 3**) | `DESIGN-REVISION-1/2`: nothing overwritten or deleted in the journal, a torn last line is repaired on append, single-writer lock | `blob.replace_atomic` (used); `blob.append` and `blob.append.concurrent_whole` (certified on `opendal-fs`); `lock.process` (implemented in `persistant-opendal/src/process_lock.rs`, **no matrix row** found) | `"0.4.4"` is `^0.4.4`, which can never take 0.9.x: the standing dependency rule is not met; measured about 11 ms per write because each write starts a worker thread and runtime (its CHANGELOG); the torn-line repair is not a PersistAnt behaviour |
| **antspawn** `e0d79ba9` | per-job spool and result files crossing a sandbox boundary (`spawn-runner.rs`, launchers) | files by design (**level 3**) | not declared | none | **not a candidate**: the files are the interface to a sandboxed child |
| **KISS** `a76aef05` | none at run time (specification and conformance harness; writes fixtures in tests) | none | not applicable | none | **not a candidate** |
| **HumboldtUnifiedKidTracker** `5eb8f520` | 27 tables, 36 migrations (ROADMAP row C3; table count from `DESIGN-KINDS.md`, not re-counted); in-memory `dashmap` caches | `sqlx 0.9` PostgreSQL with `macros` (`backend/Cargo.toml`), 883 `query!` literals (measured at `f54c05c`, `DESIGN-SQLX-BRIDGE.md`) (**level 3**, by CireSnave's ruling, board 230 option 3) | transactions (29, per Humboldt's adoption plan), 46 FKs, triggers, PostgreSQL-only SQL; compile-time checked queries are the point | `rel.transaction`, `rel.foreign_key`, `rel.migrate.*`, `sweep`, `PgBackend::from_pool`; the `sqlx_query!` family and `OnPool` / `OnConn` (0.9.17, 0.9.18) | Humboldt migrates module by module (its work); PostgreSQL array columns are loud in the driver (2 fields); no `connection()` accessor (C6, unbuilt, deliberately) |
| **mlmf** `cb3168db` | model files read (mapped) and training checkpoints written (`checkpoint.rs`: write to a temporary file, then `fs::rename`) | files and `memmap2` (**level 3**) | not declared | checkpoints would be `blob.replace_atomic` | `blob` is whole-value: `read` returns a `Vec<u8>` and `write` takes one; there is **no ranged or streamed read or write** (`persistant-blob/src/backend.rs`), so multi-gigabyte checkpoints and any mapped load cannot go through it; **not a candidate until streaming exists** |
| **lightbulb** `f42be3da` | KV-cache cold tier (bytes by key); API keys, audit logs, rate-limit counters, an unused `user_sessions` table (`AUTH_PERSISTANT_ADOPTION_FIT.md`) | the cold tier: `persistant 0.4.1`, `Store`, `Config::Fs` (`cache/tiered_storage.rs`, **level 2**); the rest: `persistant-sqlx 0.7.4` `PgBackend` with `PgTx` named in `admin.rs` (**level 3**: PostgreSQL only); `tokio-postgres` for one test | the cold tier declares `Read Write Delete AtomicReplace`; the SQL side has a document, no capability list | `blob.*` for the tier; `rel.transaction`; rate-limit counters and sessions are `kv.compare_and_swap` + ttl (or `take` / `count`) | pins `0.4.1` and `0.7.4` are behind; `Cargo.toml` says `PgBackend::connect` offers no pool acquire timeout, so it builds its own pool (`PgPoolOptions` + `from_pool`, `src/api/mod.rs:441-459`): **no gap, re-checked 2026-10-23**, `PgBackend::connect_with(&ConnectionSecret, &ConnectOptions { tls, max_connections, acquire_timeout })` exists since 0.4.31 (its default timeout is 10 s, and its default TLS policy is `Require`, which a loopback database must relax); `Value` has no `Uuid` or timestamp variant (`admin.rs:549`): `FromValue` / `ToValue` for them exist since 0.9.14, the variants still do not |
| **fuel** `c534d905` | telemetry as JSONL flushed by temporary file and rename (`telemetry/sink.rs`); a git-checked-in verification ledger embedded with `include_str!`; weights through `memmap2` | files (**level 3**) | not declared | the JSONL flush is `blob.replace_atomic`; the ledger and weights are read-only | **not a candidate**: the ledger is a source file, the weights are mapped, the sink is a batch artefact |
| **auth-framework** `ba76d952` | tokens, sessions, kv with ttl, RBAC | features `sqlite-storage`, `postgres-storage`, `redis-storage` (`sqlx`, `tokio-postgres`, `redis`), crate `auth-storage-sqlite` (**level 3**) | `docs/DESIGN-AUTH-STORAGE.md` here: swap, take, atomic batch, `ttl_remaining`, count, secondary keys | all present in 0.9.24: `compare_and_swap`, `take`, `batch_atomic`, `ttl_remaining`, `count`, `Indexed`; memory and sqlite / postgres / mysql / mariadb | **not** OpenDAL (no compare-and-delete, no batch); Redis is not a PersistAnt backend; throughput unmeasured; the lane builds the adapter |
| **ThinkersJournal.com** `b37ad2ae` | none of its own: a static Astro site and three Cloudflare Pages Functions; the newsletter form forwards to Listmonk on Neon PostgreSQL (`docs/newsletter-provisioning.md`) | none | not applicable | none | **not a candidate** (not Rust, no store) |

## 4. Ranked adoption list

Ranked by what the next step buys, not by size.

1. **auth-framework.** Unblocked today: 0.9.24 carries every capability its note asked for, on five backends. Its
   adoption also moves **commy's per-tenant databases** (item 2) off direct `sqlx`, and retires its Redis and hand-rolled
   SQLite storage. Open on its side: the adapter, a throughput measurement, and the Redis decision.
2. **commy.** Already stores its durable state through PersistAnt. Next: bump `0.8.21` to the current line and open the state
   store through the registry instead of a directory (level 2 to 1), so the operator can pick; the tenant databases follow item 1.
3. **agentlife.** Fix the pin first (`0.4.4` cannot take 0.9.x: a standing-rule breach, a one-line change), then replace
   `Config::Fs` with a registry listing for the atomic-write path. The journal could move to `blob.append` (certified on
   `opendal-fs`), but it is a deliberate file format with a torn-line repair; that is a design question for the lane, not a
   mechanical port. `lock.process` had no matrix row when surveyed; it has one from 0.9.27.
4. **lightbulb.** Two pins to raise; the PostgreSQL side is level 3 only because `PgTx` is named in `admin.rs`; the counters and
   sessions it wants are kv leaves that now exist. The pool-timeout gap should be re-checked against 0.9.24 first.
5. **synapse.** The model consumer. Remaining work is its own: close gaps 2 and 5 and drop the redb fall-back for fresh
   homes, so that no path bypasses `providers`.
6. **Humboldt.** On a ruled path (option 3, work orders A to D built); the rest is the lane's migration.

Not candidates: **commy-sdk-rust, antspawn, KISS, fuel, ThinkersJournal.com** (nothing durable of their own, or files that
are the interface). **mlmf** waits on a streaming blob API.

## 5. What this asks of PersistAnt

In order of how many projects each unblocks. None is started.

1. **Documentation and an example for the registry path** for level-2 users (`Store` + `Config`): commy, agentlife and
   lightbulb all named a backend in code because that is what the older API showed.
2. **A matrix row for `lock.process`** (implemented and tested, but not in any `matrix/*.tsv` when this was surveyed; **added in
   0.9.27** as `std-file-lock`, decided by a killed child process with a marker-file control): agentlife's `FileLock`.
3. ~~**Pool options on `PgBackend::connect`**~~ (lightbulb's stated reason for building its own pool): **already there**, `connect_with` since 0.4.31; lightbulb's comment is stale. Tell the lane (the pin is 0.7.4).
4. **Streamed / ranged blob I/O** (mlmf, and any large-object user). A new capability and a trait change; a design first.
5. **Encryption at rest.** The vocabulary reserves it as `storage.no_plaintext_at_rest` ("a canary value written through the
   layer is not found as raw bytes in the data directory, log or temp files"), with no implementation, test or matrix row
   (`DESIGN-KINDS.md` lists it among the capabilities that "have no row, so they are off"); only connection TLS exists. No
   project above *states* it as a need, so it stays a reserved leaf, not a request. *(Corrected 2026-10-23: this item first
   said the vocabulary names nothing, from a search for the word "encrypt"; the leaf is spelled differently.)*
6. **A Redis backend** only if auth-framework's operators need it; it would be native for every kv leaf in
   `DESIGN-AUTH-STORAGE.md` and is not proposed here.
7. **Windows owner-only files** (built for SQLite databases in 0.9.43; `ROADMAP.md`, "File permissions").

## 6. How to repeat it

`git -C <repo> fetch origin`, then the greps in section 1 against `origin/main`, then the per-project documents named in
the table. Each row's SHA is the state it was read at: a number or a claim here is about that commit and no later one.
