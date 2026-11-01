# Design note: `kv.in_relational_transaction` (ROADMAP "Kinds" row 6)

Status: **proposal, no code.** Written 2026-11-01 at `7565dfb` (0.16.3) on the PM's ruling (row 6 after row 9, design only).
Source of the requirement: `DESIGN-KINDS.md` 4.4 point 4 ("a transaction can span both kinds on the same engine").

## 1. The need, as the design already states it

"Token + index + the program's own row commit atomically": auth-framework's Redis path writes a token in
three or four non-atomic round trips (`DESIGN-KINDS.md` 1.3, by reading, unverified at runtime); on a
relational engine the same writes can share one transaction if `kv` operations can run inside the
caller's `RelationalTx`. Capability id `kv.in_relational_transaction`, present only when both kinds
resolve to one backend instance. **Which consumer asks for it today: none that I verified.**
auth-framework is the named case; I did not re-read its storage code for this note.

## 2. What exists (read at `e92491b`, unchanged since but for docs)

- `KvOnRelational<R>` (`crates/persistant-kv/src/on_relational.rs`) owns `db: R` and exposes
  `backend_handle() -> &R`. Each `KvBackend` method calls `self.db.execute/query` (autocommit) except
  `batch`, which does `self.db.begin(ReadCommitted)`, runs the ops through private helpers
  `tx_exec` / `tx_qry` / `claim_two_step` that already take a generic `&mut T: RelationalTx`, and commits.
- `RelationalBackend::begin(isolation) -> Self::Tx<'_>`; `RelationalTx::{execute, query, commit, rollback}`
  take `&mut self` / `self` (`crates/persistant-relational/src/backend.rs`). The tx borrows the backend (`Tx<'a>`).
- `batch_once` takes a lock row (`SELECT id FROM persistant_kv_batch WHERE id = 1 FOR UPDATE`) on every
  dialect but SQLite, so racing batches do not deadlock on gap locks. `batch` retries up to 16 times on
  `Deadlock` / `Serialization`.
- `kv.in_relational_transaction` appears only in `DESIGN-KINDS.md` 4.4: no leaf, no matrix row, no code.

So the SQL for each operation already exists in a form that runs on a borrowed transaction. What is
missing is an API, a capability, a leaf, and the decisions in section 4.

## 3. API options

| Option | Shape | For | Against |
|---|---|---|---|
| A. Methods on `KvOnRelational` taking the tx | `kv.put_in(&mut tx, key, v, ttl)`, `get_in`, `delete_in`, `put_if_absent_in`, `compare_and_swap_in`, ... generic over `T: RelationalTx` | Smallest: reuses `tx_exec` / `tx_qry` | A parallel method set to keep in step with `KvBackend`; no trait, so kind-generic code cannot use it; any `T` is accepted (section 4.1) |
| B. A `KvTx<'t, T>` adapter behind a new trait | `kv.within(&mut tx) -> KvTx<'_, T>` with `KvBackend`'s method names | One method set to learn; wrappers (`Traced`, `Faulty`) can implement the trait | Another trait; a borrowed, generic adapter does not fit the registry's `AnyKv` enum directly |
| C. Registry level | `Handle::transaction()` returns one value with `tx.kv()` and `tx.rel()`; one commit | Matches "one declaration, one refusal"; same instance by construction | Needs `AnyTx` / `AnyKv` plumbing for every backend; the biggest change |

**Recommendation: B as the engine-level API, with A as its first step; C later if the registry asks.**
A is B without the trait, so starting with A costs nothing to promote.

## 4. Semantics that have to be decided

1. **Same instance.** The kv operations must run on a tx begun from the same database whose
   `persistant_kv` table they use. With `T: RelationalTx` generic nothing stops a caller passing a tx from
   another database. Taking `&mut R::Tx<'_>` ties the tx to `R`'s type, not to the instance (two pools of
   one type still pass); a runtime token check is only as good as the token (`backend()` / `version()`
   strings are not unique). Recommend the typed signature, and C for the instance guarantee.
2. **Locking.** `batch_once`'s `FOR UPDATE` on `persistant_kv_batch` would, inside a caller's tx, be held
   until the caller commits, so every other batch on MySQL / PostgreSQL waits for the caller's whole
   transaction. Recommend the in-tx `batch` **does not take that lock** and documents that it can be a
   deadlock victim.
3. **Retry.** `batch` retries `Deadlock` / `Serialization` because it owns its transaction. A joined
   operation cannot: the server aborted the caller's whole tx. It returns the error unchanged.
4. **Isolation.** The caller chose it at `begin`. The kv semantics were certified at `ReadCommitted`; the
   claim is "at least ReadCommitted", and behaviour above that is **unverified**.
5. **TTL clock.** `NOW` is written once, in PostgreSQL form, as `clock_timestamp()` (`on_relational.rs:50`):
   wall-clock time, not transaction time, so expiry inside a long tx is judged as of each statement, as
   outside one. How the `Translator` renders it for MySQL, MariaDB and SQLite was **not read**; MySQL's
   `NOW()` is statement time and SQLite's is per statement, so the in-tx leaf should include a TTL case.
6. **Rollback is the point.** A `put` inside a tx the caller rolls back must vanish; a read in the tx sees
   the tx's own writes; a second connection sees neither until commit. Those three are the leaf.
7. **Wrappers.** `Traced`, `Faulty` and `Instrumented` forward `KvBackend`; the in-tx surface needs
   forwarding in each, or the capability is off through a wrapper (as `blob.max_value_size` was until the
   wrappers forwarded it).

## 5. Capability and test

- Leaf `kv.in_relational_transaction`, certified only for `KvOnRelational<R>` rows (postgres, mysql,
  mariadb, sqlite); `missing` for `KvOnBlob`, memory and stores that are not relational, as the design says.
- Red-first tests with known-bad defects, as for the other leaves: (a) **autocommit in tx** (the in-tx put
  runs on `db` instead of the tx): a rolled-back tx still shows the key; (b) **no tx-local visibility**
  (the in-tx read goes through `db`): the tx does not see its own put; (c) **swallowed rollback**; plus a
  positive control (commit makes the key visible to a second connection).
- On SQLite the second-connection check needs a file database, not `:memory:`; I did not check how the
  existing SQLite rows are opened.

## 6. Order of work

1. A: `get / put / delete / put_if_absent / compare_and_swap / compare_and_delete` in a tx (the token +
   index case), the leaf, rows for four engines.
2. `batch` and `ordered_scan` in a tx.
3. Trait B and wrapper forwarding.
4. Registry `Handle::transaction()` (C).

Each is one PR and a minor bump. The capability and leaf are additive; nothing is Breaking unless
`KvBackend` itself changes, which this plan does not do.

## 7. What I did not verify

- That auth-framework, or any consumer, wants this today; the need is the design's, not a measured ask.
- How `NOW` is translated for MySQL / MariaDB / SQLite (4.5), and isolation behaviour above `ReadCommitted`.
- That option B's borrow pattern (`Tx<'a>`) works in `async` code without trouble; not compiled.
- Whether the registry's kv and relational handles can share one backend instance; I did not read whether
  they share a pool.
- Nothing here was built or run.

## 8. Asks

- PM: confirm B (A first), and whether to wait for a consumer's request before work order 1.
- PM: confirm the in-tx `batch` should skip the batch lock (4.2).
