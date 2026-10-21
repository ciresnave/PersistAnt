# PersistAnt: one persistence layer, every kind of persistence

## How PersistAnt works (CireSnave, 2026-10-21, verbatim; read this before anything below)

> *"My original thinking was something like this: When someone writes a program that needs persistence, they would have their configuration UI call PersistAnt with a list of capabilities they need, and likely also an order of importance so the backends could be sorted.  PersistAnt would take that information and compare it to the capabilities of its backends and return a sorted list of backends.  The program's configuration UI could then provide that list to whoever is setting up the program and they could choose a backend.  That backend would be stored in the program's configuration.  When the program ran, it would tell PersistAnt what backend it was configured for and then use backend-agnostic calls to do whatever it needed to with that backend through PersistAnt.  That way if PersistAnt gained a new backend module, every program that used PersistAnt whose capabilities were met by that new backend would instantly have support for a new backend."*

Earlier the same day: *"PersistAnt was supposed to have a function that, when fed a list of needed capabilities, would provide a list of backends PersistAnt could provide that would satisfy all of those capabilities.  If it doesn't have that, the current design is already wrong."*

**The five-step contract**, built as `persistant::registry` (0.9.7; section 3, "Backend registration"):

1. The program's setup UI sends PersistAnt its required capabilities plus an ordered list of what matters most (`ProviderNeeds::new(..).prefer(..)`).
2. PersistAnt returns the backends that meet the required set, sorted by that order of importance, then speed (`registry::providers`); a need nothing satisfies is an error naming each backend's unmet id, never a closest match.
3. The operator picks one from the list, which says why each ranks where it does (`Listing::why`).
4. The program stores the backend's **stable id** in its own configuration.
5. At run time the program passes the id (and where: a path or URL) to `registry::open_named` and uses backend-agnostic calls only (`Handle::kv`).

**Not allowed:** a project naming a backend in its own code, or owning a registry or candidate list of backends.

Design document. Written 2026-10-11 by the PersistAnt lane for the PM's `[TASK]` of the same day;
**revised 2026-10-14** after a design conversation with CireSnave's session (§2, §3), and **built
and published as 0.4.23 the same day**: §11 says what exists, what was found by building it, and what
is not built. Sections 1 to 10 are the design as proposed and ruled; where building contradicted
them, §11 says so. CireSnave and the PM decide (decisions and rulings are in §9).

## 0. What this is, and how far to trust it

**The ruling this serves** (CireSnave, 2026-10-11, board 151 widened; archived at
`C:\Projects\CIRESNAVE-DECIDED-ARCHIVE.md`, verbatim):

> "PersistAnt should support them [databases]... We can build it hierarchically (PersistAnt ->
> PersistAnt-Blob, PersistAnt-RelationalDatabase, etc.)... If other projects aren't using PersistAnt, we
> modify our other projects so they are... each project told PersistAnt what capability set it needed...
> 'SQL alongside' risks meaning 'SQL separate' which would make the design kludgy. The SQL abilities need
> to integrate and flow as part of the whole stack."

Then, the same day, on scope and on dialect leakage (relayed by the PM, verbatim): *"At this point, I
meant Rust projects. I do plan on writing cross-language support for almost all of our projects ... but
for now that is on the eventual roadmap but not until our tools we're building actually work."* and
*"worth keeping in mind while we build but not a reason not to build it."* So: Rust projects only (§6 is
a short "do not foreclose it" section), and dialect leakage is a **stated cost** in §4, not an argument
against building.

**What the 2026-10-14 revision changed.** Capability is a boolean, off until an automated test passes
(§2.1); the vocabulary is named by feature-space coverage, not by consumer demand (§2.2); `Declared`,
`emulated` and `conditional` are gone; the crate plan is trait crates, generic adapters and driver
crates (§3); the matrix row schema, including `config`, is written down (§2.4). Anything in §1 that
still says "needed by" is evidence of use, not a gate.

**Provenance.** Facts about other projects come from read-only passes at the `origin/main` heads below,
each read with `git show origin/main:<path>` after a `git fetch` (no checkout, no refs written). Citations
are `repo:path:line @ sha`. Four research passes produced most line numbers; I re-ran these myself at
the same heads and they held: Humboldt has 32 migration files, 11 `CREATE TRIGGER` statements, and 122
`sqlx::query!` + 24 `query_as!` + 97 `query_scalar!` in `backend/src` (control: the same grep finds each
form); auth-framework's `list_kv_keys` default returns an empty list (`src/storage/core.rs:188-193`);
`sqlx::query!` is absent from auth-framework (control: `sqlx::query(` finds 23 call sites in
`src/storage/postgres.rs`); lightbulb and agentlife depend on `persistant` by crates.io version. Every
other line number is the research pass's and is marked **(pass)** where it matters; treat those as
pointers to re-read, not as proof. Anything I could not establish is under **UNVERIFIED** in §10.

| Repo | Head read | | Repo | Head read |
|---|---|---|---|---|
| auth-framework | d96c00d (0.6.0-rc43) | | OverMind | 04aa5d2 |
| HumboldtUnifiedKidTracker | b7cd95e | | consent | 9c585a9 |
| synapse | 3b58bab | | coderipper | 3f649d1 |
| lightbulb | 674b06d | | fuel | cc8b7bf |
| agentlife | 455b068 | | mlmf | 3f22047 |
| persistant | 19c0a74 (0.4.7) | | ThinkersJournal-Community | e3b6fb6 |

`origin/main` moves; a number here is a statement about that head, not about the present. The earlier
evaluation is `C:\Projects\OPENDAL-FIT-2026-10-08.md` (cited as **FIT**); where this refresh disagrees
with it, §1.3 says so.

## 1. Consumer inventory, refreshed

### 1.1 Who persists what

Only Rust projects are in scope; TypeScript projects are listed in §6 for completeness.

| Project | Persists | Backend today | Guarantees the code relies on |
|---|---|---|---|
| auth-framework | tokens, sessions, KV with TTL (`AuthStorage`); audit, MFA, roles (traits, mostly **no** persistent impl) | Postgres/SQLite via sqlx 0.9.0 (runtime strings, no macros), Redis, memory, DashMap, encrypted wrapper | key lookup, per-record expiry, secondary lookups by user and by access token, prefix list, count, upsert; **no** multi-key transaction (`core.rs:76-197`; `src/storage/postgres.rs:42-143` @ d96c00d) |
| Humboldt (Rust backend) | 27 tables: roster, dismissal events, accounts, access log, secrets | Postgres only, sqlx 0.9.0, 32 forward-only migrations | transactions, 46 FKs, 11 triggers, expression/partial unique indexes, `FOR UPDATE`, advisory locks, compile-time-checked queries (§4) |
| synapse | mailbox (queues, leases, acks), keystore, session cache, announce file, security log; legacy Postgres layer (**since removed**: synapse origin/main 6b8ad2e has no sqlx or redis use (the synapse lane, 2026-10-17: `git grep` for `sqlx::|redis::` finds 0, redb finds mailbox.rs)) | redb 4.3.0 (format 2); files; sqlx 0.8.6 + redis 0.32.4 behind features (**since removed**) | one atomic write transaction per mailbox operation, ordered composite-key scan, time-index sweep, 0600 files, no-clobber publish, OS file lock (`src/mailbox.rs:11-12,636-674`; `src/keystore.rs:581-624`; `crates/synapse-client/src/lib.rs:586-707` @ 3b58bab, pass) |
| lightbulb | KV-cache cold tier (bytes by key); API keys, audit logs, user sessions, auth-attempt counters | `persistant` 0.4.1 on fs (`Cargo.toml:125`); Postgres via tokio-postgres 0.7 + deadpool-postgres + refinery 0.9 | atomic replace; UNIQUE, FKs, `ON CONFLICT`, expiry filter plus 300 s sweep; no explicit transactions (`src/cache/tiered_storage.rs:58-146`; `src/api/mod.rs:305-352` @ 674b06d, pass) |
| agentlife | agent registry (file per agent), pending restores, journal, plans | `persistant` 0.4.4 for atomic replace only (`Cargo.toml:44`); own files | temp+sync+rename, create-new lock with stale reclaim and owner token, append with torn-line repair (`src/atomic.rs`, `src/lock.rs`, `src/restore.rs:653-722`, `src/journal.rs:124-174` @ 455b068, pass) |
| OverMind / consent | grants, attempts, hash-chained audit, vault, lane state, restart log | files (one crate set lives in **both** repos, identical blobs) | OS file lock held for the store's lifetime, multi-file consistency with a chain verified on open, DPAPI key, atomic replace, plain append |
| coderipper | build cache (directory per key), tool cache with SHA-256 pins | directory trees; advisory `try_lock` | a real directory handed to cargo, LRU prune; tool install is tmp+`sync_all`+rename |
| fuel | telemetry JSONL, hardware probe, transfer cost, dispatch table, training checkpoints | files | tmp+rename in four places, none with fsync |
| mlmf | hub download cache (`.part`+rename), checkpoints, exports | files, mmap for model reading | atomic only when `atomic_save` is set |
| SafeImage | nothing: sans-I/O by design; defines `CaseRecord`, `ReportState` the host must store | none | a future consumer of typed records (`src/evidence/case.rs:76`, `src/report/state.rs:58`) |
| KISS, Unpopped, baracuda, candlelight, auto-discovery, amt, aocl, vulkane, ThinkersFoundation | no runtime persistence (build-time file writes only) | none | none |
| Solulos | README only, no `.rs` files | none | none |
| FAM | (TypeScript on Bun `bun:sqlite`; scheduled for decommission) | n/a | n/a |

### 1.2 What the whole inventory says

These are the findings the design below is built on. Each is a count or a quote, not an impression.

1. **Duplicated mechanisms are the majority of the surface.** Hand-written atomic replace: 17 sites in 8
   repos (about 13 distinct once identical blobs are merged); only 6 of the 17 sync the file before
   the rename (user-request x2, synapse x3, coderipper; pass). Create-new lock with stale reclaim: 4 implementations, only one with an
   owner token. OS file locks: 6 sites. Append with torn-line repair: 1, against 9 plain-append sites.
   TTL/retention sweeps: 6 independent implementations. Whole crates are duplicated across repos
   (`user-request`, `with-secret`, `lane-restart`, identical blob hashes) — a portfolio problem, noted,
   not solved here.
2. **Two of the biggest consumers do not need what OpenDAL gives.** Humboldt is relational and uses no
   blob or KV store at all. auth-framework's real need is KV + TTL + ordered prefix list + a few atomic
   primitives (§1.3).
3. **Only one consumer needs relational integrity enforced by the database:** Humboldt. lightbulb's five
   tables and auth-framework's SQL are plain CRUD with indexes and upserts.
4. **Postgres is the production relational engine everywhere** (Humboldt, lightbulb, auth-framework default
   feature; synapse's legacy layer was removed, see the survey row). SQLite appears as a second SQL backend only in auth-framework
   (in-tree and the unpublished `auth-storage-sqlite`) and, at the survey commit, in synapse tests.
5. **The driver stack is already fragmented.** sqlx 0.9.0 (Humboldt, auth-framework), sqlx 0.8.6
   (synapse, at the survey commit; since removed), tokio-postgres + deadpool + refinery (lightbulb), tokio-postgres alone for auth-framework's
   migration CLI with `NoTls` hard-coded (`src/migrations/mod.rs:280`, pass). redis 0.32.4 (synapse, since removed) vs
   1.7.1 (auth-framework lock). Three migration mechanisms across four projects: sqlx's checksummed
   ledger, refinery, and hand-rolled `CREATE TABLE IF NOT EXISTS` / a version-only table.
6. **Only two repos depend on `persistant` today**, both by crates.io version, both behind main
   (lightbulb 0.4.1, agentlife 0.4.4; main is 0.4.7). Both left residue next to the dependency:
   lightbulb's scratch-dir lock, agentlife's lock, journal append and a per-call scratch-dir workaround
   (`src/atomic.rs:5-10,28-75` @ 455b068, pass). That residue is the measured list of what the crate is
   still missing for its first two users: locks and append.

### 1.3 Where this refresh corrects the earlier evaluation (FIT)

| FIT said | Now at the head read |
|---|---|
| lightbulb `FileDiskStore` is a bare `fs::write` (A.4) | It is `persistant::blocking::Store` with atomic replace since 674b06d. FIT also **missed lightbulb's Postgres layer entirely** (it was present at the old head too). |
| agentlife `write_atomic` is hand-written (A.6) | Goes through `persistant` since 037c7fe (#27). |
| auth-framework Redis uses `expire` on the user set and `SCAN` (A.2) | That is `src/storage/redis.rs`, which no config path reaches. The factory builds the `RedisStorage` in `core.rs:388,645`: `KEYS` scans, **no** `EXPIRE` on the user set, no session index, three-to-four non-atomic round trips per token write (pass; the reading was by file listing, UNVERIFIED at runtime). |
| sqlx 0.8.6 in auth-framework | 0.9.0 now (`Cargo.toml:397`). synapse was the one on 0.8.6 at the survey commit and no longer uses sqlx. |
| Part A.2 lists several storage traits with backends | FIT already noted no `MfaStorage` impl. This adds: `SessionStorage` has **no** impl either; `AuthorizationStorage` is memory only; `RoleStorage`'s `DatabaseStorage` needs a `DatabaseConnection` that nothing implements; audit has no persistent backend (`src/audit.rs:310-319,401,434-460`). Only `AuthStorage` is durable. |
| Humboldt: "32 migrations", 27 tables | Confirmed 27 tables, 11 triggers, 32 migrations; **46** FKs by statement count (PM said 45; UNVERIFIED against a live `pg_constraint`). |
| OverMind `StoreLock` etc. at ~1613-1660 | `store.rs:1706-1751`; the old line numbers are stale. |

Defects found in auth-framework along the way are **not this document's to fix**, but they are what the
KV contract in §2 is shaped by, so they are listed once, with the evidence, because a shared backend
must decide each deliberately (pass, d96c00d): SQL backends return expired tokens/sessions from `get_*`
until `cleanup_expired` runs, while Redis does not and `MemoryStorage` never expires tokens at all
(`dashmap_memory.rs:406,132`); `list_kv_keys` silently returns empty on three implementations;
SQLite truncates TTL to whole seconds (`sqlite.rs:238`); three mutually incompatible Postgres
`sessions` schemas, and `PostgresStorage::list_user_sessions` selects columns its own `migrate()` never
creates (`postgres.rs:533`, by reading, UNVERIFIED at runtime); user uniqueness is check-then-set and
`users:index` is a read-modify-write JSON list (`src/auth_modular/user_manager.rs:477-526`); single-use
consume is `get` then `delete` (`src/server/oauth/par.rs:268-300`).

## 2. One capability vocabulary across kinds

### 2.1 The shape

A **capability** is a named property of a backend that a program may depend on. The vocabulary is one
flat, namespaced list (`blob.*`, `kv.*`, `rel.*`, `lock.*`, `storage.*`, `ops.*`, ...). A **program
declares needs** at open; the gate looks up the backend's capabilities; opening is refused, with every
unmet need named, when they do not cover the declaration. That is today's `Need`/`Needs`/`Error::Refused`
(`src/store.rs:17-63`, `src/error.rs:21-28` @ cc934e1), generalised from one kind to all.

**Revised 2026-10-14.** An earlier version of this section gave a capability a status (`Proven`,
`Declared`, `Absent`) and a "how" (`native`, `emulated`, `conditional`). Both are removed. The model is:

1. **A capability is a boolean** for a `(backend, version, config)`, where `config` is only what the gate
   can read at open. **It is off until a test passes.** "Not set" is the default and the only way to be
   off; there is no "declared" middle. A program that needs it and finds it off is refused.
2. **Verification is always an automated test.** People and LLMs write and review tests; nobody
   asserts a capability. Untestable means it stays off. Environment beyond what the gate can read is
   covered by "works on my machine" with the environment recorded as metadata (§2.3, §2.4).
3. **Four outcomes of a test**, and what each does to the vocabulary:
   - *exact* (the result matches the capability's written contract) → **on**;
   - *equally correct but different* (pi as `3.141592` where the test expected `3.14159`) → widen the
     test **only if the test was stricter than the capability's written contract**; otherwise the
     difference is a **new leaf** with its own contract, plus a generalisation over the leaves;
   - *wrong* → **off**, and a bug report to the backend's developer;
   - *missing* → **off**.
4. **Leaves, chains, alternatives, composites.** A leaf names one behaviour or feature, **never a
   backend**. Ordered refinements are a **chain**: the stronger leaf implies the weaker. Unordered
   variants are an **OR** over leaves. A **composite** is derived from its leaves *and* the
   composite's own contract test must also pass on every route that derives it: derivation says
   *eligible*, the test says *on*. Test sets form a hierarchy matching the vocabulary.
5. **Ids never change meaning.** A change in meaning is a new id. Each leaf carries a **written
   contract** (a sentence, such as a comparator's canonicalisation), and **any test change that turns
   a failing backend green must cite the contract wording** that justifies it, else the test was bent to
   fit the backend.
6. **Every test has a positive control and a known-bad control.** The positive control shows the test
   can pass on something that is right; the known-bad one shows it can fail on something that is
   wrong (examples in §2.2 and §3 #5).
7. **The vocabulary is data.** Ids are stable strings with a serialisable schema, so a manifest of what a
   program needs can be written down, diffed and checked outside Rust (§6), and adding an id is not a
   breaking change (`#[non_exhaustive]` stays, ids are append-only). UniSQL and PersistAnt agreed
   2026-10-12 that the ids are plain strings owned by PersistAnt, that UniSQL does not import
   `persistant-core` (at most a dev-dependency drift test), and that each repo owns its matrix *file*
   while this crate owns the row *schema* (§2.4).
8. **A need may carry a parameter** (maximum value size, dialect and minimum major version).
   `Need::MaxValueSize(u64)` is the existing example; the parameter is compared with the value the
   test measured.

### 2.2 The vocabulary, named by coverage

**The vocabulary is named by the feature space, not by who needs it.** This corrects the first version of
this section, which admitted an id only when a consumer needed it. A vocabulary that grows by consumer
request grows by accident, and every request is a design review. Instead:

- **Relational ids follow the SQL standard's feature codes**, so "what does this database give" is
  answerable from a published list. PostgreSQL documents its conformance by those codes (its appendix
  states it supports "at least 170 of 177 mandatory Core features"); that list is approximate and only
  its introduction was read here, so SQLite's and MySQL's would be separate surveys (§10).
- **Blob and KV ids follow OpenDAL's capability fields** (about 100) plus what the consumers in §1 do
  that those fields do not name (create-exclusive *with* content visible, file mode).
- **Untested is off**, so a wide list is safe: an id with no passing test claims nothing. The table
  below is the **seed** from the consumers seen so far, not the boundary.
- **Needs should be derived, not hand-written**: UniSQL's `analyse` reads a SQL string and returns the
  constructs it uses, which map to `rel.*` leaves; a program's needs for its whole statement set can be
  computed. Whether Humboldt's checked-in `.sqlx` files are a complete statement set is UNVERIFIED.

"Consumer seen" below is evidence of use, not a gate. A capability here is **boolean** (§2.1);
"usually given by" says where it is typically on, which a test must still confirm.

| Id | Meaning | Consumer seen (evidence, not a gate) | Usually given by | Known not given / caveat |
|---|---|---|---|---|
| `blob.read` `.write` `.delete` `.stat` | bytes by key | lightbulb, agentlife, all file writers | fs, memory, s3 (OpenDAL) | — |
| `blob.list` | ascending, paged, literal prefix | agentlife registry listing (`registry.rs:420-429`); auth KV prefix list | memory, s3, fs (directory scan) | prefix filtering done by this crate on OpenDAL (today) |
| `blob.replace_atomic` | reader sees old or new, never a mix | 17 hand-written sites (§1.2) | fs with scratch dir, memory | refused without `atomic_write_dir` |
| `blob.create_exclusive` | write only if absent | agentlife/lane-restart locks, synapse keystore | fs `create_new`, s3 if-not-exists | **not together with `replace_atomic` on fs via OpenDAL** (README, 0.4.7) |
| `blob.create_exclusive_atomic` | exclusive *and* content fully visible on creation | synapse `write_new` by `hard_link` (`keystore.rs:606-615`) | a native fs path (hard link) — **not OpenDAL** | no OpenDAL service advertises it |
| `blob.durable` | survives power loss (file and parent directory synced) | synapse and user-request sync the file; fuel, mlmf, lane-restart do not | none via OpenDAL (no parent-dir fsync, README *Limits*) | on only if the VM hard-reset test passed (below) |
| `blob.file_mode` | owner-only permission on create | synapse (keystore, session cache, announce file, security log: 0600; keystore dir 0700) | native fs path | not in OpenDAL's capability struct (FIT §C row 10) |
| `blob.append` | append bytes, optionally with torn-line repair | agentlife journal; 9 plain-append sites | fs | repair is ours |
| `blob.max_value_size(n)` | one value up to n bytes | D1 1 MB ceiling (FIT §B.1) | — | refused only where a ceiling is advertised |
| `kv.get` `.put` `.delete` | typed or byte values by key | auth-framework `store_kv/get_kv/delete_kv` | Postgres, SQLite, Redis, redb, memory | built by this crate over blob (record envelope) |
| `kv.ttl` | per-write expiry; expired entries invisible to get **and** list; ms resolution | auth `store_kv(…, ttl)` (`core.rs:142`), OAuth/OTP/PAR windows; user-request `expires_at` | Redis, Postgres/SQLite (column + predicate) | blob: `expires_at` in the envelope + sweep (needs `blob.list`) |
| `kv.ttl_remaining` | read back the remaining ttl | auth `get_kv_ttl` (`core.rs:157`), migration/backup re-store | Redis, SQL | built from the envelope by this crate |
| `kv.put_if_absent` | atomic create | auth user-uniqueness (`user_manager.rs:477-503`); lock acquire | Redis `SET NX`, SQL `INSERT … ON CONFLICT DO NOTHING`, redb | blob: only where `blob.create_exclusive` is native |
| `kv.compare_and_swap` | atomic update if unchanged | `users:index` (`user_manager.rs:514-526`) | SQL, redb | blob: only with if-match (s3) |
| `kv.take` | get and delete atomically | single-use PAR/codes (`par.rs:268-300`) | SQL (`DELETE … RETURNING`), Redis `GETDEL` | — |
| `kv.batch_atomic` | several keys all-or-nothing | auth token + access-token + user index; synapse mailbox contract | SQL, redb, Redis `MULTI` | **not on blob**; refused |
| `kv.ordered_scan` | ascending bytewise range/prefix, paged | synapse `(role, seq)` scan; auth prefix list | redb, SQL (`ORDER BY`), blob (sorted by this crate) | Redis: `KEYS` is neither ordered nor bounded; refused (Redis can give the weaker `kv.prefix_list` below) |
| `kv.prefix_list` | given a literal byte prefix and a page limit: at most `limit` keys per call that begin with the prefix, with a continuation; every key present for the whole listing is returned **at least once**; order unspecified; keys written or deleted during the listing may or may not appear; a TTL-expired entry is invisible to the listing as well as to `get` | auth-framework `list_kv_keys` (agreed with its lane 2026-10-14; ADR-011, auth-framework#162; its tests in #145) | Redis `SCAN` + `MATCH`; anything that gives `kv.ordered_scan` (chain: `kv.ordered_scan` implies it) | tests: glob metacharacters `* ? [ ] \` in the prefix match literally (known-bad: an unescaped `MATCH` pattern), empty prefix, `limit` 1 over 1000 keys returns all via the continuation, an expired key absent while a live sibling is present, a key that is a prefix of another is returned once |
| `kv.prefix_list.exactly_once` | as `kv.prefix_list`, and no key is returned twice | none yet (auth-framework de-duplicates and does not need it) | anything that gives `kv.ordered_scan`; not Redis `SCAN` | a caller that cannot tolerate repeats names this leaf and is refused on Redis |
| `kv.count` | count under a prefix | auth `count_active_sessions` | SQL | scan |
| `lock.lease` | exclusive lease with owner token, expiry, safe reclaim | 4 hand-written locks (§1.2) | built on `kv.put_if_absent` + `kv.ttl`, or on `blob.create_exclusive` | built by this crate; on only if its lock test passes |
| `lock.process` | held until the process dies, no staleness | user-request, synapse RoleLock, coderipper, baracuda (6 sites) | local fs only | not offered remotely |
| `rel.dialect(d, min)` | the program writes SQL for dialect `d` and the backend *is* `d` at version >= `min` | Humboldt, lightbulb, synapse, auth-framework | Postgres | see §4 |
| `rel.source_dialect(d)` | statements written in `d` are accepted and **translated** for this backend (UniSQL); each construct is a leaf (`cast.to_uuid`, `upsert.where`, ...) | UniSQL; agreed with its lane 2026-10-12 | a backend with a UniSQL dialect crate | a construct UniSQL refuses is simply not on; it never turns into a status |
| `rel.transaction` | begin/commit/rollback | Humboldt (`routes/accounts.rs:115`), migrations | Postgres, SQLite | none |
| `rel.isolation(level)` | read committed / repeatable read / serializable | Humboldt uses `REPEATABLE READ READ ONLY` (`transfer.rs:12`) | Postgres | — |
| `rel.savepoint` | nested rollback | Humboldt `remove_accounts.rs:192-203` | Postgres, SQLite | — |
| `rel.foreign_key` | enforced references (with action) | Humboldt 46; lightbulb | Postgres, SQLite (if enabled) | — |
| `rel.check_constraint` | row-level invariants | Humboldt ~41 live | Postgres, SQLite | — |
| `rel.unique_index.expression` / `.partial` | unique on `lower(trim(name))`; unique where predicate | Humboldt 3 + 5 | Postgres, SQLite (both) | — |
| `rel.trigger` | database-side invariants and state machines | Humboldt 11 (7 are invariants) | Postgres, SQLite (reduced) | no equivalent in KV/blob |
| `rel.row_lock(for_update)` / `.skip_locked` | pessimistic row locks | Humboldt 8 sites; TJ-Community `SKIP LOCKED` | Postgres | SQLite lacks both |
| `rel.advisory_lock` | named locks inside a transaction | Humboldt 6 sites + trigger 0030 | Postgres | — |
| `rel.upsert(where)` | `ON CONFLICT DO UPDATE … WHERE` | Humboldt `dismissal_events.rs:862-868`; auth, lightbulb | Postgres, SQLite (no `WHERE` on conflict target variants: UNVERIFIED) | — |
| `rel.json` / `rel.array` | JSONB, `TEXT[]` | Humboldt 3 + 4 columns | Postgres | — |
| `rel.bulk_copy` | `COPY` in/out | Humboldt backup/transfer (`transfer.rs:392,403`) | Postgres | — |
| `rel.session_setting` | transaction-local setting readable by triggers | Humboldt erasure escape hatch (`data_deletion.rs:49-59`) | Postgres | — |
| `rel.catalog` | read the schema (triggers, functions) | Humboldt install check (`dbtools.rs:207-257`) | Postgres | — |
| `ops.pitr` | point-in-time recovery restores to a chosen instant | PM's list; **not data-path**, but testable | PostgreSQL base backup + archived WAL; verified by restoring and comparing | on only if the test (restore to instant *t*, compare to a snapshot taken at *t*) ran on that environment |
| `storage.no_plaintext_at_rest` | a canary value written through the layer is not found as raw bytes anywhere in the data directory, WAL and temp files included | auth-framework's encryption-at-rest design (`PORTFOLIO-ROADMAP.md:505`) | an encrypted volume or engine-level encryption | positive control: the same search on an unencrypted instance **must** find the canary |

**How the hard-to-observe ones are tested** (each is an automated test; people and LLMs author and
review tests, never "verify" a capability by assertion):

- **Power-loss durability** (`blob.durable`): write, acknowledge, hard-reset a VM, mount, read. It needs
  the virtual disk to pass flushes through, and a positive control: an *unsynced* write must be **lost**
  in the same run, or the test proves nothing. Not run today; no OpenDAL service passes it for `fs`
  (README *Limits*: no parent-directory fsync).
- **Concurrency** (`blob.create_exclusive`, `kv.put_if_absent`, `lock.*`): a stress test with recorded
  trial parameters (threads, iterations, duration). A race that did not occur in N trials is "no
  failure seen", which is what a passing test is; the parameters go in the row. Known-bad control:
  OpenDAL `fs` with `atomic_write_dir` set, where exclusive create is check-then-rename (README 0.4.6
  and 0.4.7), must fail the test.
- **Point-in-time recovery** (`ops.pitr`): base backup, archived WAL, restore to an instant, compare.
- **Encryption at rest**: raw-bytes canary search above.
- **Retention** (`kv.ttl`): a short TTL and a real sleep suffice (no clock manipulation); known-bad
  control: an entry written without TTL must still be visible afterwards.

**Rule for adding an id.** Ids are named by what the *feature space* contains, not by who asked
(§2.2 intro). An id nobody uses costs nothing, because it is off until a test turns it on. A consumer
sets which tests are written first, not which ids exist.

### 2.3 Verification

- **The gate reads facts, not behaviour.** At open it learns the backend, its version, and any
  configuration the gate can read cheaply (a locale, a collation, a directory is set). It then *looks up*
  the row for `(backend, version, config)` (§2.4). It does not decide behaviour itself. **An unknown
  version has no row, so every capability is off** — the user's phrase for it was "exactly the right
  cost": the price is an update to the matrix, not an unverified guess.
- **Where tests run.** (a) In CI, against each shipped driver and each supported server major (Humboldt's
  CI already starts `postgres:16`: `ci.yml:153-160` @ b7cd95e, pass), producing the checked-in matrix.
  (b) In a consumer's own CI, with the same suite against *its* production backend, so "the backend gives
  what I need" is a failing build, not a discovery at 3 a.m. That is the one mechanism that would have
  caught every inconsistency in §1.3's auth-framework list.
- **A live test at open** (rolled-back `TEMP` objects, an advisory lock, `FOR UPDATE SKIP LOCKED`) would
  be one more place a test runs, needing the `TEMP` privilege. It is not required by this design and is
  **decision D3**; **UNVERIFIED** that every deployment's role has the privilege.
- **Blob and KV tests** write, read, list, race and delete under a scratch prefix, as the current suite
  already does against the real `fs` service for atomic replace (README *Scope* §2).
- **What "works on my machine" covers.** Behaviour that depends on the environment beyond what the gate
  can read (filesystem type, disk cache, kernel) is verified once on the machine that ran the test, and
  the environment is recorded as metadata (§2.4). That is a statement about that environment, not about
  all of them, and the row says so by carrying it.

### 2.4 The matrix row

One recorded fact per row, in a checked-in tab-separated file per repo (UniSQL's are at
`crates/unisql-dialect-*/matrix-<backend>-from-<source>.tsv`); a reader merges files. The row
schema is owned here:

| Field | Meaning |
|---|---|
| `subject_kind` | `capability` or `construct` (UniSQL's constructs map onto `rel.*` leaves) |
| `subject` | the leaf id |
| `backend` | the driver or dialect crate's backend name |
| `version` | backend (server) version; no row for an unknown version means **off** |
| `config` | `k=v;k=v` facts the gate can read at open (a locale, a collation, `case_sensitive_like`). A test that passes only under a fixture config is a row for **that** config |
| `enabled` | `yes` or `no` (boolean, §2.1) |
| metadata | environment (OS, filesystem type), backend version string, trial parameters (§2.2), date, test id, note |
| `reason` | for `no`: `missing`, `wrong` or `untested` |

- **A row exists per test that ran**; absence of a row is `untested`, off.
- **A divergence a program may accept** is minted as its *own leaf* with its own contract (§2.1 #3). The
  note carries `allowance=<name>` so UniSQL's caller-allowance name and the leaf are linked; a program
  that accepts the divergence names that leaf in its needs, so the gate stays boolean.
- **A known-bad run** (for example the MySQL default collation for an `Exact` leaf) is also a row, with
  `enabled=no, reason=wrong`.

## 3. The crate hierarchy

CireSnave's own sketch is `PersistAnt -> PersistAnt-Blob, PersistAnt-RelationalDatabase, etc.` This
section fills it in. **Proposal, not a decision.** The split below came from a conversation with
CireSnave's session on 2026-10-13: *trait crates* define a kind, *adapter* code builds one kind on
another, *driver crates* own the third-party dependency. Crate names were free on crates.io when read
2026-10-13 (`persistant` returned 200 as the positive control; the six below returned 404). Every crate in
the project carries one version number (CireSnave's standing rule, `C:\Projects\CLAUDE.md` §9).

```
persistant                (facade: re-exports; features pick kinds and drivers; keeps today's API)
├── persistant-core       (vocabulary, Needs, boolean capability sets, matrix row schema and reader,
│                          error taxonomy, config + secret handles, conformance harness, tracing
│                          field names. No I/O dependencies.)
├── persistant-blob       (BlobBackend trait + its contract tests. No driver.)
├── persistant-relational (RelationalBackend trait, deliberately SMALL: run a statement with
│                          parameters, begin a transaction, report dialect and capabilities, hand out
│                          the concrete driver handle. No driver.)
├── persistant-kv         (KvBackend trait + contract tests; generic adapters KvOnBlob<B: BlobBackend>
│                          and KvOnRelational<R: RelationalBackend>; memory. No driver.)
├── persistant-opendal    (implements BlobBackend: fs, memory, s3; today's persistant code moves here)
└── persistant-sqlx       (implements RelationalBackend: Postgres, then SQLite; pool, TLS policy,
                           checksummed migrations, typed error classification, test harness)
```

**Rules for the hierarchy.**

1. **No kind crate depends on a driver.** A program that wants only blobs compiles no database driver;
   one that wants KV on Postgres compiles `persistant-kv` + `persistant-sqlx` and no OpenDAL. A new
   driver (another blob service, another database client) is a new crate and touches no kind crate.
   (`persistant` today pulls `opendal` with `default-features = false, features = ["services-fs"]`,
   `Cargo.toml` @ cc934e1: the same discipline.)
2. **Adapters are generic and live with the kind they provide.** `KvOnBlob` and `KvOnRelational` are in
   `persistant-kv` and are **wrapper types**, not blanket `impl KvBackend for T`, because Rust has no
   specialisation and a blanket impl would forbid a native KV backend for the same type.
3. **An adapter's capabilities are derived as *eligible*, then tested.** `kv.put_if_absent` is eligible
   on a relational backend that has a unique constraint with conflict handling; it is *on* only for a
   `(backend, version, config)` row whose KV contract test passed (§2.4). The same holds for `KvOnBlob`
   (`kv.put_if_absent` only where `blob.create_exclusive` is on).
4. **The relational trait stays small.** If it grows toward statement builders or row mapping it becomes
   Option B (§4.2) and carries B's leaks. Humboldt keeps `sqlx::query!` through the concrete handle the
   trait hands out; the trait exists so the KV adapter, the capability matrix and the migration ledger
   have one thing to call, not to hide sqlx.
5. **The KV adapter's SQL is written once, in a source dialect (Postgres), and translated by UniSQL**
   per target. It does not carry a hand-written statement per dialect. If UniSQL refuses a construct for
   a target, the KV capability that depends on it is off on that target, and the refusal is the row's
   reason; there is no per-dialect fallback table. **Traps the KV contract suite must include** (each
   with a known-bad run, §2.1): PostgreSQL text `ORDER BY` follows the database's collation, and MySQL's
   default collation (`utf8mb4_0900_ai_ci`) is case-insensitive, so keys must be `BYTEA`/`BLOB` or a
   binary collation or `kv.ordered_scan` is wrong in a way that passes a lower-case test.
   **Found by running the suite on MySQL 8.4 (2026-10-16), now pinned by tests:** (a) sqlx-mysql always
   connects with `CLIENT_FOUND_ROWS`, so an upsert that changed nothing reports 1, the same as an
   insert, and `ROW_COUNT()` agrees; "stored or not" cannot come from an upsert's count, so MySQL's
   insert-if-absent is `UPDATE` (rows *matched*) then a plain `INSERT` whose duplicate-key error means
   "live" (`claim_two_step`; test `an_upsert_cannot_say_whether_it_stored`). (b) InnoDB's default
   REPEATABLE READ gap-locks a locking read or an `UPDATE` that matches nothing, and two such
   transactions inserting into the same gap deadlock (`kv.put_if_absent` went off with "deadlock
   detected"); MySQL transactions run at READ COMMITTED.
6. **Core has no I/O.** So a program that only wants to *read a manifest of needs* or the recorded matrix
   (a CI check, a future TypeScript client, §6) depends on `persistant-core` alone.
7. **The facade keeps `persistant::Store` working** for lightbulb and agentlife: the blob API is
   re-exported unchanged until a breaking minor says otherwise (pre-1.0 the *second* number is the
   major, CLAUDE.md §9).
8. **MSRV.** `persistant` was 1.91 (OpenDAL's) until 0.5.0, when it became 1.94: UniSQL 0.24 and sqlx 0.9.0 are
   both 1.94 (FIT §D.d, from the crates.io API) and `persistant-kv` translates through UniSQL. CireSnave permitted raising
   a project's MSRV on 2026-10-08 (`CLAUDE.md` §9); each crate declares its own.
9. **UniSQL is a dependency of the adapter, not of the traits**, and it is not on crates.io (§7).
   Nothing in `persistant-core`, `-blob`, `-relational`, `-opendal` or `-sqlx` needs it.

**Backend registration.** *(Rewritten 2026-10-21. It used to say there is no global registry and that the facade's `open` matches a URL scheme; that left "which backends satisfy my needs?" unanswerable, which CireSnave ruled wrong, see the top of this file.)* The registry is a **table compiled into the build** from the enabled features (`registry`: `memory`, `fs`; `registry-sqlite`: `sqlite`, `sqlite-full`; `all-backends`: every one). Each entry is a provider: a stable string id, the matrix rows it is looked up under (version and configuration come from the recorded `matrix/*.tsv`, embedded), its declared durability, and the endpoint it needs (none, a path, a URL). It is not mutable at run time and not populated by the program, so "what can this binary do?" is still a fact of the binary, not an accident. The program never names a backend in its own code: it asks `registry::providers(&needs)` and later `registry::open_named(id, endpoint, &needs)`, and both go through the same gate (needs against the recorded rows, boolean, off unless a test passed).

- **Ids are plain strings a program stores in its configuration. They are append-only and never change meaning**; a changed meaning is a new id (a test pins the list). Driver and server versions are not in the id; the gate checks the backend actually opened against the recorded rows and refuses with the unmet ids.
- **A new backend module reaches every program whose needs it meets** when the program upgrades `persistant` and rebuilds. There are no dynamic plugins (nobody asked for them); a program pinned to an old `persistant` does not see it.
- A consumer-private backend that implements the kind trait is still used directly, outside the registry, through the same traits; it does not appear in the sorted list.
- The handle serves the key-value kind in this release (`Handle::kv`, the closed enum `AnyKv`); a need for blob or relational ids is reported unmet until the handle serves them.
- Why a closed enum and not a boxed trait object: `KvBackend` returns `impl Future`, so it is not object-safe; an enum needs no second trait, no allocation per call, and its variants are exactly the drivers the build includes.

**Where blob metadata and KV rows live is configuration, not code.** One Postgres instance can hold a
program's relational schema and its KV table. That is the integration the ruling asks for and §4.4 makes
it concrete.

## 4. The relational API: three options and what each leaks

### 4.1 The two consumers, as measured

**Humboldt** (b7cd95e). 27 tables, 46 FKs (4 with `ON DELETE`: 3 CASCADE, 1 SET NULL), 11 triggers,
14 functions, 2 views, ~41 live CHECKs, 3 expression-unique and 5 partial indexes, 2 `NOT VALID`
constraints (`inv-humboldt` §1). No extensions, RLS, enums, `LISTEN/NOTIFY`, window functions or
`SKIP LOCKED`. `sqlx` 0.9.0 with 243 compile-time-checked query sites in `backend/src` (122 `query!`,
24 `query_as!`, 97 `query_scalar!`; 763 more in tests), 119 runtime ones concentrated in dynamic admin
tools; 566 committed `.sqlx` files; CI builds with `SQLX_OFFLINE=true` and starts `postgres:16` for 499
`#[sqlx::test]` functions. The Rust code depends on database-enforced rules **by constraint name** at
≥7 sites (`routes/zones.rs:68-76`, `routes/bus_routes.rs:29-39`, `routes/staging_areas.rs:127-133`,
`routes/dismissal_events.rs:874-881`, `routes/webauthn_routes.rs:197-206`, `routes/district.rs:113-118`;
pass), relies on a trigger whose escape hatch is a transaction-local setting
(`routes/data_deletion.rs:49-59`), and shares an advisory-lock key string with a trigger
(`routes/parent_auth.rs:140-143` and migration `0030:21`). The database is **Postgres by design**:
catalog reads, `COPY`, `DISABLE TRIGGER`, a CI pin to 16.

**auth-framework** (d96c00d). The storage trait is KV + records + a few lookups; SQL is 23-27
runtime-string statements per backend (`postgres.rs`, `sqlite.rs`, `auth-storage-sqlite/src/lib.rs`),
no macros, no `.sqlx`, no checksummed migrations (§1.3). Its relational-shaped need that has **no
backend yet** is `AuditStorage::query_events` (11 filters, count, statistics, retention;
`src/audit.rs:258-278,461-486`).

### 4.2 The three options

**Option A — typed repository traits per consumer.** The consumer defines `ZoneRepo`, `AuditRepo`;
PersistAnt supplies the pieces around them (transaction/unit-of-work handle, error classification,
backends' pool and migrations) and each backend implements the consumer's traits.

- *Leaks.* Every backend adapter re-implements "which constraint was violated?" and the
  case/trim-insensitive uniqueness of Humboldt's three expression indexes. Trigger state machines (7 of
  11 triggers) must move into the adapter or the domain layer and be re-tested per backend. Multi-table
  transactions (the dismissal write + log row + `FOR UPDATE`, `dismissal_events.rs:449-472`; the 18-DELETE
  erasure) need a unit-of-work that per-table traits do not give. Advisory-lock keys shared with a trigger
  have no portable form. Compile-time checking survives *only inside* the Postgres adapter.
- *Humboldt.* ~90% of the 243 compile-time sites are single-table CRUD behind route modules and would
  map; the cost is above. Net: a rewrite that buys nothing while Postgres is the only backend.
- *auth-framework.* This **is** what `AuthStorage` already is, and the evidence in §1.3 is what it costs
  when five implementations of one trait share no contract and no tests. A alone does not fix that.
- *Dialect leakage.* Concentrated in the adapters, where it is invisible to the consumer until the
  second backend exists.

**Option B — a query builder (sea-query / SeaORM / diesel style).** PersistAnt exposes a dialect-neutral
builder; each backend renders it.

- *Leaks.* The builder checks Rust types, not column existence, so Humboldt's schema-checked queries
  (243 + 763 sites) become unchecked or move to Diesel's `schema.rs` model, a different mechanism.
  `DISTINCT ON`, `FOR UPDATE OF`, `jsonb_object_keys`, `make_interval`, `hashtext`, `set_config`, `TEXT[]`
  and `COPY` all need raw fragments, i.e. the builder is leaky exactly on the hot paths. DDL (expression
  indexes, partial indexes, triggers, functions) stays raw SQL migrations regardless. Types differ
  (Postgres `INET`/`JSONB`/`TIMESTAMPTZ` vs SQLite storage classes). The `_sqlx_migrations` ledger that
  Humboldt's transfer tool compares by checksum (`transfer.rs:9-10`) ties it to sqlx's migrator.
- *Humboldt.* Loses compile-time checking where it matters most; a second dialect would not make the
  triggers portable.
- *auth-framework.* Fits: its statements are simple, so one builder would give Postgres + SQLite from
  one description and remove the duplicated SQL. This is B's one clear win here, and it is a win that
  KV-on-relational (§4.4) also delivers without a builder.
- *Dialect leakage.* Visible at the raw fragments and the type map, one per call site.

**Option C — a thin typed pass-through over sqlx.** PersistAnt does **not** abstract queries. It owns
what surrounds them: connection configuration with secret handles and a TLS policy, pool settings,
checksummed migrations, typed error classification, the capability declaration and probe, a test
harness, tracing. It hands the consumer a `Pool`/`Transaction` of sqlx's own types, so `query!` and
`#[sqlx::test]` keep working.

- *Leaks.* Zero query portability: a program that writes Postgres SQL declares `rel.dialect(postgres)` and
  any other backend is refused at open. The sqlx types are in the public API of this one crate — which
  departs from the blob crate's rule of no OpenDAL types in the public API. That is deliberate:
  hiding sqlx's types is exactly what would lose compile-time checking. sqlx 0.9 → 0.10 is
  then a breaking change for the relational crate's consumers; the cost is one crate, not N.
- *Humboldt.* Loses nothing it uses. Gains: the TLS policy it currently leaves to the URL's `sslmode`
  (`db.rs:4-10` has no timeouts and no `ssl_mode`), pool timeouts, one place for the constraint-name
  classification, the capability probe replacing hand-written `KNOWN_TRIGGERS`/`KNOWN_FUNCTIONS`
  (`dbtools.rs:188-219`) checks, a shared test harness.
- *auth-framework.* Gives it one pool/TLS/migration story; fixes the `NoTls` migration CLI, the three
  schemas and the missing checksums. Does not by itself fix KV semantics (that is §2's contract).
- *Dialect leakage.* **All of it stays with the consumer**, openly: a SQLite deployment of a
  Postgres-SQL program is refused, not emulated.

### 4.3 What a portable layer would lose for Humboldt, said plainly

1. **Compile-time-checked queries.** 243 + 763 sites. Capability declaration cannot express this: it is
   a *build-time* property that needs a live database or the committed `.sqlx` cache (sqlx README:
   "The `DATABASE_URL` environment variable must be set at build time"; sqlx-cli: "Check this directory
   into version control and an active database connection will no longer be needed"; fetched
   2026-10-11). It survives only if the consumer still calls sqlx's macros against sqlx's own types, which
   is Option C and nothing else.
2. **Database-enforced invariants.** 7 of 11 triggers plus ~14 cross-column CHECKs plus the three
   case-insensitive unique indexes enforce rules the Rust code *relies on* rather than re-checks. A
   portable layer must re-implement them in Rust as the single enforcement point, re-tested per backend,
   or refuse any backend that lacks them. Declaring `rel.trigger`, `rel.check_constraint`,
   `rel.unique_index.expression`, `rel.session_setting`, `rel.advisory_lock` makes that refusal *honest*:
   the program states it is not portable to anything that cannot give those, and the suite verifies the
   one backend that claims to.
3. **Postgres-only features in live use:** advisory locks (6 sites + a trigger), `FOR UPDATE` (8),
   `ON CONFLICT … DO UPDATE … WHERE`, `DISTINCT ON`, JSONB (3 columns), `TEXT[]` (4), `COPY`, catalog
   introspection, `set_config`. None has a KV or blob equivalent.

So for Humboldt the honest declaration is `rel.dialect(postgres, 16)` plus the fine-grained list in
§2.2, and its portability is **zero by declaration**. That is not a defect of the layer; it is the layer
refusing to pretend.

### 4.4 Recommendation, and where "integrated, not alongside" is real

**Ruled 2026-10-14 (board 181, relayed by the PM; CireSnave, verbatim): "On decision 181: C+".** C+ is
Option C below plus the KV kind implemented natively on a relational backend, so auth-framework gets
Postgres and SQLite from one description. PersistAnt owns connection, TLS and secret handles, the pool,
checksummed migrations, error classification and the capability test. "Natively" is `KvOnRelational<R>`
in `persistant-kv` (§3 #2) with its capabilities tested per `(R, version, config)`, not assumed.

**Recommended Option C as the relational API, with KV implemented natively on the relational backend** (this
is what was ruled).
Options A and B stay available to any consumer *on top of* C (auth-framework's future audit store is the
natural place for a narrow typed repository), but PersistAnt does not ship them: they carry the leaks
above without removing the one that matters here.

The ruling warns that "SQL alongside" would mean "SQL separate". These are the points where C is
integrated into the same stack, each of which is a concrete, testable property:

1. **One declaration, one refusal.** A program that needs `kv.ttl` and `rel.foreign_key` declares both in
   one `Needs`; one `open` refuses or accepts. No separate "database config" path.
2. **Relational is also a good KV backend.** `persistant-kv` offers `KvOnRelational<R>` for any
   `RelationalBackend` `R` (a wrapper type, not a blanket impl, since Rust has no specialisation): a
   table with `(key, value, expires_at)` where TTL, put-if-absent, CAS, take, batch and ordered scan are
   *derivable* from the relational capabilities, which blob backends cannot give (§2.2). "Derivable" is
   not "on": each KV capability is on for a given `(R, version, config)` only after the KV contract
   tests pass there (§3). auth-framework's token/session problems (§1.3) are solved by this, not by a
   builder.
3. **One pool, one config, one migration ledger.** The KV table is created by the same checksummed
   migration framework as the program's own schema, over the same pool and TLS policy.
4. **A transaction can span both kinds on the same engine.** `rel.transaction` hands back a transaction
   that `kv` operations accept (capability `kv.in_relational_transaction`, present only when both
   resolve to one backend instance). That is what lets token + index + the program's own row commit
   atomically — the multi-key atomicity the Redis path lacks. **Leak:** it exists only on one engine;
   on a split deployment the capability is absent and a program that declared it is refused.
5. **One error taxonomy.** sqlx errors are classified into the same data-carrying enum as blob/KV
   errors: `UniqueViolation { constraint }`, `ForeignKeyViolation`, `CheckViolation`, `Serialization`,
   `Deadlock`, `Unavailable`. Humboldt's ≥7 hand-written constraint-name mappings become one.
6. **One fake/harness story**, with the honest limit in §5.

## 5. Features that must be one thing

| Feature | Why one, and the design | Evidence it is wanted |
|---|---|---|
| **Migrations with checksums** | One ledger concept for SQL schema and for non-SQL record rewrites (resumable, idempotent, ordered, checksummed, refuses a newer stamp). Must **keep sqlx's `_sqlx_migrations` table shape** so Humboldt's transfer tool, which compares versions *and* checksums (`transfer.rs:9-10`), keeps working: constraint, not preference. | Three SQL mechanisms in four repos (§1.2 #5); auth-framework has no checksum (`src/migrations/mod.rs:41-205`); synapse runs `sqlx::migrate!` **and** a hand-rolled table (`storage/migrations.rs:46-76`) plus a redb format stamp (`mailbox.rs:760-819`) |
| **Test fakes and fault injection** | Today: memory, `Masked` (pretend a capability is missing), `Faulty` (fail or interrupt the Nth write) for blob. Extend `Masked` and `Faulty` to every kind. **Limit, stated:** there is no honest fake of Postgres semantics. The relational harness is a real ephemeral database (what `#[sqlx::test]` already does for Humboldt, 499 tests) with fault injection at the statement/commit boundary; an in-memory SQLite is offered only as a backend that *declares* a smaller capability set, so a Postgres-SQL program is refused rather than falsely passing. | synapse `FailingStore`/`CrashingStore`, lightbulb `FlakyStore` (FIT §D.f); Humboldt tests assert trigger refusals |
| **Secret-handle connection config** | A DSN is never a bare `String` in a `Debug`-printable struct: it is a handle (env var name, file, or a `with-secret` reference) resolved at open and redacted in every format. | Humboldt reads `DATABASE_URL` with `expect` (`config.rs:8-9`); auth-framework's `StorageConfig::Postgres { connection_string }` is a plain field (`config/mod.rs:89-107`); whether it is `Debug`-printed anywhere is UNVERIFIED |
| **Typed refusal** | Exists (`Error::Refused { backend, missing }`, `src/error.rs:23-28`): `Missing` carries the capability id. A capability is boolean (§2.1), so the refusal says which ids were off and, as data, the row's reason (`missing`, `wrong`, `untested`, §2.4). Errors are plain data (no `Box<dyn Error>` in variants a client must read). | §6 |
| **Observability** | One set of `tracing` span/field names (`persist.kind`, `persist.backend`, `persist.op`, `persist.capability`), no value or key *content* by default (keys can be secrets), counters left to the consumer's subscriber. | consumers already use `tracing` (auth-framework); nothing exotic wanted |

## 6. Do not foreclose other languages (later, not now)

Scope is Rust. This section only lists design choices that would make a later TypeScript or other
binding **hard or easy**; it is not an options analysis and has no cost estimate.

**Easy, so keep:**
- Capability ids and `Needs` as stable, serialisable data (§2.1), so a manifest of needs can be checked
  outside Rust and checked against a recorded conformance matrix (§2.3).
- Errors as data: a closed set of kinds plus fields, no Rust-only payload types. The present enum is
  already close (`src/error.rs`).
- Configuration as URL + options, not Rust types.
- `persistant-core` with no I/O dependency, so a non-Rust client can share the schema files.
- A recorded conformance matrix as a checked-in artifact rather than something known only at run time.

**Hard, so avoid where a choice exists:**
- Public APIs that expose generic type parameters, lifetimes or trait objects to callers where an owned
  value would do (a `Record` trait with an associated schema number is fine for Rust; a wire form is
  `{kind, schema, bytes}`, which the envelope already is).
- Capabilities that exist only as Rust traits with no id.
- Async-only without a synchronous door. `persistant::blocking` already exists (README *Synchronous
  callers*); keep one for every kind.
- Panics and global state in the kind crates.

Why not more now: the PM's relay of CireSnave is explicit that this waits "until our tools we're
building actually work". For context only, the TypeScript research found no schema shared between TS and
Rust anywhere and `HANDOFF.md:87` of ThinkersJournal-Community recording "workers-rs was non-viable"
(pass; UNVERIFIED by me).

## 7. Adoption order (Rust projects)

Each project's lane migrates itself; PersistAnt supplies the capability and the contract tests. Order is
biggest benefit for least risk, and each step names the capability it needs that does not exist yet.

| # | Project | What moves | Needs from PersistAnt | Risk | Benefit |
|---|---|---|---|---|---|
| 1 | lightbulb, agentlife (already consumers) | Catch up from 0.4.1 / 0.4.4; delete the residue (lightbulb's scratch lock, agentlife's `FileLock`/`RestoreLock` and temp-name workaround) | `lock.lease`, `blob.append` (with repair), an `atomic_write_dir` that no longer needs a per-call scratch directory | low: both lanes already ship it | removes ~4 hand-written locks and the append repair |
| 2 | Atomic-replace duplicates: OverMind/consent (`store.rs:1922`, `vault.rs:113`, `lane_state_writer.rs:420`), agentlife lane-restart copy, fuel (4 sites), mlmf (2), coderipper tool cache, synapse (3) | `blob.replace_atomic` | `blob.file_mode` and `blob.create_exclusive_atomic` for synapse (they publish by hard link, 0600) | low-medium: Windows rename behaviour untested (UNVERIFIED) and `consent`/OverMind hold identical copies, so do one and diff | most duplicated code in the portfolio; fixes the four fuel sites that never fsync |
| 3 | auth-framework `AuthStorage` KV | `persistant-kv` backends (Postgres/SQLite/Redis/memory) beneath the existing trait; tokens/sessions as key schemes with `kv.ordered_scan`; user uniqueness via `kv.put_if_absent`; single-use via `kv.take` | `kv.*` and `kv.batch_atomic`, `persistant-kv` + `persistant-sqlx` | **medium-high**: security-critical; mid-release (rc43 → stable 0.6.0 with an *encryption-at-rest storage-interface change still to design*, `PORTFOLIO-ROADMAP.md:505`) | the largest correctness gain in §1.3 |
| 4 | Humboldt | `persistant-sqlx` Option C: config/TLS/pool/error classification/probe/harness; **keeps** every `query!` | `rel.*` probes, secret handles | low if C; it is a thin wrapper | closes the TLS and pool-timeout gaps; one shared migration ledger |
| 5 | lightbulb Postgres (5 tables) | from tokio-postgres + deadpool + refinery to the sqlx-based crate | `persistant-sqlx` | **medium**: a driver swap for a small schema; no transactions to port | one stack; refinery goes |
| 6 | ~~synapse legacy Postgres~~ **gone** (synapse origin/main 6b8ad2e has no sqlx or redis use (the synapse lane, 2026-10-17: `git grep` for `sqlx::|redis::` finds 0, redb finds mailbox.rs)) | nothing to move | - | - | row retired |
| 7 | auth-framework audit store | a first relational audit backend, where a typed repository (Option A) over C may be right | `persistant-sqlx` | low: nothing exists to regress | the only relational-shaped need with no backend |

**Not migrated, and why:**
- **synapse mailbox (redb).** Its contract is one atomic multi-key transaction with ordered composite-key
  scans (`mailbox.rs:11-12,251-258`), and CireSnave recorded "Redb is fine for now" (FIT §A.3). It is a
  *candidate* for `kv.batch_atomic` + `kv.ordered_scan` after step 3 has run in anger; not before.
- **OverMind/consent `user-request`.** OS file lock held for the store's lifetime, a three-file chain
  verified on open, DPAPI: `lock.process` is local-only and the chain is the consumer's own integrity
  rule. Its *atomic-replace and append sites* are in step 2; the store as a whole is not.
- **coderipper build cache, mlmf/fuel model files, baracuda/vulkane/aocl build-time writes.** A real
  directory handed to another tool, memory-mapped multi-GB files, and build scripts: not persistence
  in this sense. coderipper's *tool cache install* (`tools/mod.rs:654`) is in step 2.
- **SafeImage.** Sans-I/O by design; it becomes a consumer only when a host stores its records.
- **KISS, Unpopped:** nothing persisted. **FAM:** TypeScript, being decommissioned.

**Cost the PM should price.** Steps 3 and 4 depend on `persistant-relational` and `persistant-sqlx`,
which do not exist. Step 1 and 2 depend on locks and append, which `HANDOFF.md` of this lane deferred
"until a consumer names it": the measured residue in §1.2 #6 is that naming.

**A publication dependency.** The KV adapter over any relational backend (§3) translates its statements
with UniSQL. A `persistant-*` crate may depend on another repository only by a crates.io version
(`C:\Projects\CLAUDE.md` §9, *Sources*), and UniSQL is a private, unpublished repository today, so
that adapter cannot be *shipped* until CireSnave publishes UniSQL. It can be designed and tested against
`unisql::analyse` from a path or commit in the meantime, in a branch that is not released. The blob
adapter, the blob driver, and the native-SQL path do not wait on it.

## 8. Non-goals

Written down so a later request is recognised as a change of scope, not a bug.

1. **Not an ORM and not a query language.** No `Model`, no relationship mapping, no portable SQL (§4).
2. **Not provisioning.** Create, wipe, transfer and backup of databases belong to the
   `provision-database-postgresql` stack (CireSnave: "lifecycle portions of tools should be part of
   provisioning"; the stack's name is the PM's). PersistAnt never performs lifecycle. It may *test*
   a lifecycle-dependent capability such as `ops.pitr` against an environment it is handed (§2.2), and the
   capability is on only if that test passed. The provisioning repo is not on this disk (UNVERIFIED that
   it exists under that name).
3. **No cross-engine transaction.** Atomicity spans only operations on one backend instance (§4.4 #4).
4. **No hidden consistency.** A capability a backend does not give is off, and a program that needs it is refused;
   there is no silent downgrade (the founding rule, README *Scope* §1).
5. **No second implementation of a database's own invariants.** Triggers and constraints stay in the
   database; the layer *declares and verifies* that they exist, it does not re-implement them.
6. **Not a cache, queue, search engine or message bus.** `kv.ttl` is expiry, not eviction policy.
7. **No distributed consensus, replication or sharding.** Those are properties of the backend.
8. **No mmap, directory trees handed to other tools, or OS-level process locks over the network.**
9. **No Parquet/DataFusion/Delta** until something queries an append-only log (FIT §D.f stands).
10. **No automatic migration of existing deployments' data.** Importing auth-framework's current
    `auth_tokens`/`sessions`/`kv_store` tables, and encrypted-KV envelopes bound to key bytes as AAD
    (`encryption.rs`, pass), is a per-consumer task with a stated import path.
11. **No non-Rust binding in this phase** (§6).
12. **Not a way to avoid learning the backend.** Choosing Postgres, redb or fs is still a design
    decision; the layer makes the consequence of the choice visible and testable.

## 9. Open decisions

Each will go to the PM as its own `[ASK]`, one at a time, with options and a recommendation.

| # | Decision | Options | Recommend |
|---|---|---|---|
| D1 | Relational API shape (§4) | A repositories; B builder; C thin sqlx pass-through; C + KV-native-on-relational | **RULED 2026-10-14: C+ (C with KV native on relational)** |
| D2 | Crate layout and names (§3) | the trait / adapter / driver split (core, blob, relational, kv; opendal, sqlx); one crate with features; blob-only crate plus a relational crate | **RULED 2026-10-14 (PM, reversible until first publish): the split**, 7 crates, one shared version; `persistant` as facade, no kind crate depends on a driver. The PM claims the six new names with 0.0.0 placeholders; this lane does not publish them |
| D3 | Open-time verification (§2.3): is a live run of a test against the target (rolled-back `TEMP` objects) allowed at open, or is the gate a lookup in the recorded matrix only | matrix lookup only; matrix + opt-in live test; live test always | **RULED 2026-10-14 (PM): matrix lookup by `(backend, version, config)` is the gate; a live test at open is opt-in per program via its Needs; always-live rejected.** Unknown version stays off |
| D4 | One relational driver: sqlx only (moves lightbulb and auth-framework's migration CLI off tokio-postgres) | sqlx only; sqlx + tokio-postgres adapter | **RULED 2026-10-14 (PM): sqlx only**; add a tokio-postgres driver crate later only if a consumer shows the need (driver crates are additive under D2) |
| D5 | Sequencing against auth-framework's 0.6.0 stable and its encryption-at-rest storage redesign | design the storage-interface change **with** `persistant-kv` now; after stable ships; never | **RULED 2026-10-14 (PM): design it with `persistant-kv` now.** Done as a mapping (§11): auth-framework's technical answer is that it needs `kv.ttl_remaining` and `kv.purge_expired` (built), not `kv.batch_atomic`, and that envelope encryption sits above the kv trait |
| D6 | First build slice | locks + append in blob; the capability vocabulary, row schema and conformance harness in `persistant-core`; `persistant-relational` + `persistant-sqlx` skeleton | **DONE (§11).** The order actually followed: core, blob, opendal, concurrency harness, append, kv, lease, process lock, KvOnBlob over fs, relational + sqlx + KvOnRelational, matrix files, ttl_remaining + purge_expired |

## 10. UNVERIFIED

- The `provision-database-postgresql` repository does not exist on this disk (`ls -d C:\Projects\*provision*`
  returned nothing); I did not search elsewhere.
- Humboldt's FK count (46) is a statement count, not a live `pg_constraint` read; CHECK count is ±1.
  Production database provider and TLS mode are not in the repo.
- Rename atomicity over an existing destination on Windows for every hand-written copy in §7 step 2.
- Whether the `TEMP` privilege is available to every deployment role (§2.3).
- Whether SQLite supports `ON CONFLICT … DO UPDATE … WHERE` in the forms Humboldt uses, and which of the
  `rel.*` capabilities it passes: not run.
- `persistant` 0.4.5-0.4.7 publication state at the time of reading: crates.io listed 0.4.1 and 0.4.4
  (API read 2026-10-11). Re-read it when quoting.
- The auth-framework behaviours marked "by reading" (the Postgres `list_user_sessions` column failure,
  `rediss://` without a TLS feature) were not executed. Nothing in this document was built or run.
- The `persistant-*` names were free on the crates.io API when read; availability at creation time is
  not guaranteed.
- Whether a `with-secret` reference is the right secret-handle backend: not evaluated.
- Whether Redis `KEYS`-free ordered scan is achievable on the Redis backend: assumed no (§2.2).
- Whether a live open-time probe that creates `TEMP` objects is acceptable and permitted for every
  deployment role (§2.3, D3): not established.
- The SQL standard's feature-code list (the coverage basis for `rel.*`, §2.2): only the PostgreSQL
  documentation's conformance appendix introduction was read ("at least 170 of 177 mandatory Core
  features"); its per-code list, and the SQLite and MySQL surveys, were not. OpenDAL's roughly 100
  capability fields were counted from its documentation, not re-listed here.
- ~~Whether `sqlx`'s generic-over-`Database` bounds permit a small `RelationalBackend` trait~~
  **Resolved by building (§11):** the trait does not mention sqlx's `Database`; it uses a GAT
  transaction type and a narrow `Value`, and `persistant-sqlx` implements it for PostgreSQL.
- Whether UniSQL can express the KV adapter's statements (upsert-and-return, `DELETE ... RETURNING`,
  `ON CONFLICT`) for every target: its analyser exists (`unisql::analyse`, main@37182e1 = 0.0.3); the
  adapter statements were not run through it.
- Whether a VM hard reset is practical in CI for the power-loss test (§2.2): not tried.
- The `persistant-opendal` and `persistant-sqlx` names were 404 on the crates.io API on 2026-10-13
  (`persistant` 200 as the control). UniSQL's crate name and publication are CireSnave's; it is a private,
  unpublished repository today.

## 11. Status: what was built, what building found, what is not built

Written 2026-10-14 at main 0.4.24 (0.4.23 is on crates.io: `persistant`, `persistant-core`,
`-blob`, `-relational`, `-kv`, `-opendal`, `-sqlx`). Citations are crate paths at that commit.

### 11.1 What exists

| Crate | Contents | Certified by |
|---|---|---|
| `persistant-core` | `CapabilityId`, `Config` (`k=v;k=v`), `Row`/`Matrix` (the §2.4 file: read, write, merge, the gate lookup), `Verdict` (an `on` row needs both controls), seed vocabulary with a contract per id | unit tests with negative controls |
| `persistant-blob` | `BlobBackend` (read, write, delete, stat, list, create_exclusive, replace_atomic, append), `MemoryBlob` with injectable defects, `conformance::certify`, a runtime-agnostic concurrency harness | itself, against defects |
| `persistant-opendal` | `OpendalBlob` (fs, memory), `ProcessLock` (`lock.process`, std only) | `matrix/opendal.tsv`; a child-kill test |
| `persistant-kv` | `KvBackend`, `KvOnBlob` (strict or explicit in-place put), `KvOnRelational` (PostgreSQL SQL, BYTEA keys), `MemoryKv`, `Lease` (`lock.lease`) | `matrix/opendal.tsv`, `matrix/sqlx-postgres.tsv` |
| `persistant-relational` | the small `RelationalBackend`/`RelationalTx` trait, `Value`, `Row`, `RelError` (SQLSTATE classification) | via `persistant-sqlx` |
| `persistant-sqlx` | `PgBackend` over sqlx 0.9 (`rust-version = "1.94"`, only this crate), `PgBackend::pool()` for `sqlx::query!` | a PostgreSQL 16 service container pinned by digest |
| `persistant` | the original OpenDAL facade, **unchanged**; it does not yet use the crates above | its own tests |

`matrix/*.tsv` are the recorded results. CI regenerates them on Linux and fails on any difference.

### 11.2 What building found (each is asserted in a test, not argued)

1. **`fs` cannot give an atomic replace and an exclusive create together** (OpenDAL's atomic path
   checks, then renames). So `KvOnBlob` over `fs` gives an atomic `kv.put` *or* `kv.put_if_absent`,
   never both. The native relational backend gives both, which is what C+ needed.
2. **A silent downgrade was in my own adapter.** `KvOnBlob::put` fell back to a plain write where the
   blob store had no atomic replace; a reader could see half a record and nothing in the row said
   so. It is now strict, with `with_in_place_put` as an explicit choice recorded as `put_mode`.
3. **`lock.lease` cannot be built on blob** (safe reclaim of an expired lease needs
   compare-and-delete). It moved onto `kv` and is real on PostgreSQL only.
4. **OpenDAL `fs` on a case-insensitive filesystem (Windows, default macOS) treats keys differing
   only in case as one key**, and a file `a/B` collides with a directory `a/b`. `blob.read` and
   `kv.get` are off there. Found by the first Windows CI run, not predicted.
5. **The literal-prefix test must avoid `*` and `?`**: they are not valid in a Windows file name.
6. **A text key column orders by collation**: shown on PostgreSQL (`a/a` before `a/B` under
   `en_US`), and the adapter's BYTEA keys do not. A case-insensitive order is a known-bad control.
7. **OpenDAL's `memory` service has no append.**
8. **Sampled races are samples.** For `fs` with `atomic_write_dir` the driver refuses
   `create_exclusive` on known behaviour, so a lucky run cannot turn it on.
9. **Clock-dependent leaves flake on a stalled machine** (one spurious `kv.ttl` off on a correct
   server); they get three attempts, and a defective backend fails every one.

### 11.3 Not built (so off, or unanswered)

- **SQLite and MySQL backends.** `KvOnRelational` writes PostgreSQL SQL and refuses any other
  dialect. The translator is UniSQL, which is private and unpublished; a `persistant-*` crate cannot
  depend on it until CireSnave publishes it (§7). `unisql::Schema` and `analyse_with` exist; the
  catalog reader that fills a `Schema` is planned in `persistant-sqlx` and not written.
- **Migrations with checksums** (§5), **TLS policy and secret-handle connection config** (the URL is
  a plain string today), the **typed-error vocabulary beyond SQLSTATE classes**.
- **Tests that need a crash or a power cut:** `blob.replace_atomic` as a whole, `blob.durable`,
  `ops.pitr`, `storage.no_plaintext_at_rest`. They have no row, so they are off.
- **`kv.batch_atomic`, `kv.take`, `kv.count`**: not offered. auth-framework declined `batch_atomic`
  (its design needs no multi-key atomicity); `kv.take` is a single `DELETE ... RETURNING` on
  PostgreSQL and not yet asked for.
- **`blob.file_mode`, `blob.create_exclusive_atomic`, `blob.max_value_size`**: no test yet.
- **Migrating consumers** (§7 steps 1 to 7). The `persistant` facade has not moved onto these
  crates; that needs a design note and a ruling, because it changes a published API.
- **Verification by other lanes' suites.** Synapse's mailbox suite proves composite behaviour, not
  leaf ids; mapping its case names to leaf ids waits for it to merge and is not asserted from here.
