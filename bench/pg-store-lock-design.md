# Row 47: replace the one global batch lock with per-key (striped) locks (design, before any code)

Evidence (bench/pg-store-lock-variant.txt, committed with this note): on PostgreSQL 16, skipping only the `SELECT id FROM persistant_kv_batch WHERE id = 1 FOR UPDATE`
statement in `KvOnRelational::batch_once` takes the store-token ratio at 8 workers from 12.5x to 2.9x and makes it stop growing with workers. The cost of that row is a serial section:
every batch on a database waits for every other. This is a design change to batch locking; nothing below is built.

## Why the lock exists (do not weaken it unknowingly)
`batch_once` reads preconditions (`Require`) that may name rows that DO NOT EXIST. A lock on a missing row (a MySQL gap lock, a PostgreSQL in-flight insert) does not exclude a
second batch, so two batches took locks in opposite orders and the server killed one as a deadlock victim (the comment at `batch_once`). The global row makes the order irrelevant by
serialising everything. Any replacement must keep two properties: (A) two batches that touch a common key never run their check-then-write sections at the same time; (B) lock acquisition order is a
total order, so no two batches can wait on each other.

## Proposal
Keep `persistant_kv_batch`, but as N stripe rows (ids 0..N-1, N = 256, created at open exactly as id 1 is now). A batch computes the set of stripes of every key it names (Require, Put,
Delete, DeleteErased): `stripe = hash(key) mod N` with a stable hash (not `DefaultHasher`; FNV-1a over the key bytes, so every process agrees). It dedups and sorts that set ascending and takes
`SELECT id FROM persistant_kv_batch WHERE id = $1 FOR UPDATE` ONE STATEMENT PER STRIPE in ascending order, before the first precondition read. Stripe rows always exist, so there is no
missing-row case for the lock itself.
- (A) holds: two batches sharing any key share that key's stripe, so the second waits for the first's COMMIT.
- (B) holds: ascending-id acquisition from a fixed set is a total order; a cycle is impossible among batches. (A multi-row `WHERE id IN (...) ORDER BY id FOR UPDATE` is NOT used: PostgreSQL does not
  promise to lock in the ORDER BY order, so it would not give (B).)
- Batches over disjoint stripe sets run concurrently, which is the point. Two unrelated keys collide with probability about 1/N (for a 3-key batch about 3/N per other batch).
- Plain `put/get/delete` still take no batch lock (as today); they only ever hold one row, so they cannot form a cycle with a batch.
- SQLite: unchanged (its write transaction already excludes; no stripe statements).
- `bounded_call_probe` (line ~427) blocks on "the row every batch locks first"; it moves to stripe 0 and the probe's own batch uses a key hashing to stripe 0 (a constant key is chosen and asserted).
- An upgrade path: existing databases hold only id 1; open inserts missing stripe ids with `INSERT ... ON CONFLICT DO NOTHING` (the same statement shape as id 1 today), so an old row set needs no migration step.

## What it changes, and what it does not
Changes: batches on disjoint keys no longer exclude each other (isolation between them was never promised: each is one transaction at READ COMMITTED). Adds up to K-1 extra lock statements for a K-stripe
batch (K <= 3 for an indexed replace). On this box a round trip is about 0.5 ms, so the 1-worker cost rises by that much per extra stripe: expected, to be measured, not assumed to be free.
Does not change: batch atomicity (still one transaction), the Require semantics, insert-or-reclaim-expired for absent keys, erased deletes, the retry in `batch` (kept as the backstop).

## Which tests prove nothing weakened (the control, before and after)
Existing: `kv.batch_atomic.rollback`, `kv.batch_atomic.concurrent_read`, `kv.batch_atomic.optimistic_sequence`, `kv.batch_atomic.absent_race` (racing batches on a missing key) in
`crates/persistant-kv/src/conformance.rs`, plus `kv.bounded_call` and the `Indexed` tests. They must pass unchanged on SQLite, PostgreSQL and MySQL/MariaDB.
New, written before the change and run against the CURRENT code first (they must pass there; the control that the change is the only thing that moves):
1. Same-key race: N workers run `Require(absent) + Put` on one key through batches; exactly one wins (this is `absent_race` generalised to W=8).
2. Opposite-order race: two batches name keys {a,b} and {b,a} with absent preconditions, repeated 200 times; assert zero `BatchFail::Retry` (count the retries; this is the deadlock the old comment describes, so a
   stripe scheme that skipped ordering would show retries here).
3. Disjoint concurrency: two batches on keys in different stripes, one held open mid-transaction by a test hook; the other must complete (proves the lock is no longer global). Mutation: force every key to stripe 0 and assert this test FAILS.
4. Mutation for ordering: acquire stripes in descending order for one of the two batches and assert test 2 shows retries or a failure.
Then rerun `pg_store_contention` (same pairs, W=1/2/4/8) for the effect.

## Expected effect (a prediction, refutable)
Store at W=8 near the StoreNoLock figure (about 2.9x, within the measured IQR 2.7 to 3.0), W=1 slightly above 3.75x by the extra lock statements. Refuted if W=8 stays above about 6x (then stripe collisions or
the row locks on persistant_kv rows themselves are serialising, which this note's variant did not test, because the variant also removed the only lock).

## Not decided here
N (256 is a guess; the number of distinct keys per batch bounds the collision rate, and a pool of 16 connections bounds the concurrency, so 64 may do), MySQL behaviour (gap locks on the precondition reads
remain; the stripe lock is what excludes there, but I have not measured MySQL), and whether `Indexed` batches should pre-sort their keys. Build only after this note is committed and the new tests exist.
