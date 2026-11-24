# Row 30 follow-up: why does "store token" go from 2.9x (1 connection) to 10.6x (8 workers)? (design, written before the run)

Observed (bench/pg-vs-auth-sql.txt [in the next PR], bench/pg-vs-auth-sql-concurrent.txt [in the next PR]): store token = a `get` then one `Indexed::replace`
batch. Reads (token check, session, kv get) keep their ratio under 8 workers; only the store moves.

## Hypothesis
`KvOnRelational::batch_once` (crates/persistant-kv/src/on_relational.rs, "One batch at a time on a server with row locks")
runs `SELECT id FROM persistant_kv_batch WHERE id = 1 FOR UPDATE` first on every non-SQLite batch. Every batch on a
database therefore holds ONE shared row lock from that statement to COMMIT, so batches run strictly one at a time, each
costing about 6 statements (BEGIN, lock, require, 3 puts, COMMIT). The direct upsert is one statement and parallelises
across the 16 pool connections. Prediction: persist time per call does NOT fall as workers rise (it stays near its
1-worker value), direct time per call falls, so the ratio rises roughly with the speed-up the direct side gets (2.9 x ~3.6
= ~10.5 would fit; that number is a consistency check, not a proof).

## What would refute it (the measurement)
Same interleaved pairs, Store only, at W = 1, 2, 4, 8 workers (join on one thread, shared pool of 16, 25 calls per worker
per side), n = 15 pairs each (5 warm-up discarded), load recorded per pair as before. Three observations:
1. **Scaling.** persist ns/call at W=8 divided by at W=1. Hypothesis: about 1 (no gain). Refuted if it falls below ~0.6.
2. **Wait events.** A second connection samples `pg_stat_activity` every 10 ms during the persist batches and counts
   (wait_event_type, wait_event) of other backends in state active / idle in transaction. Hypothesis: most samples at W=8
   are `Lock/transactionid` or `Lock/tuple` (a backend waiting on the `persistant_kv_batch` row). Refuted if lock waits are
   a small share (under ~20%) of sampled non-idle backends.
3. **Discriminating control, `put_only`.** The same record written by one plain `kv.put` (no batch, so no global lock)
   against the same direct upsert. Hypothesis: its W=8 ratio stays near its W=1 ratio (it scales like direct). If put_only
   also jumps at W=8, the lock row is not the (whole) cause.
Controls: control_same (direct on both sides) must centre on 1 at every W; control_slow must fall well below 1.

## Not tested here
Changing the layer (a per-key or striped lock, optimistic retry); that is a design change, not a measurement. Other
workers counts, MySQL, a request mix.
