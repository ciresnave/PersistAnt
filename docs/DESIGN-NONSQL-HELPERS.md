# Migrations, locks and expiry for non-SQL stores (ROADMAP row 19)

Status: **design, 2026-11-22; nothing built from it yet.** "Non-SQL" here is the blob stores (OpenDAL `fs`,
`memory`) and `kv-on-blob`. Facts below are read from `matrix/opendal.tsv` at OpenDAL 0.59.5 (main a3a1e8a),
not tested for this document.

## What the blob stores have, and lack

| Capability | OpenDAL rows (`opendal-fs`, `kv-on-blob`) |
|---|---|
| `blob.create_exclusive` | yes (a hard link from a finished scratch file, since 0.9.41) |
| `kv.put_if_absent`, `kv.ttl`, `kv.ttl_remaining` | yes (an expired key reads as absent) |
| `lock.process` | yes (`File::try_lock`, one host) |
| `kv.compare_and_swap`, `kv.compare_and_delete` | **no** (missing) |
| `lock.lease`, `lock.fencing`, `kv.purge_expired`, `kv.take`, `kv.rate_window` | **no** (missing) |

The rule set by row 41 holds here: where the primitive is on, a typed refusal elsewhere, **nothing approximated**.

## Locks: stay refused

A lease needs to take over an expired holder. On a blob store that is "delete the stale lease, then
`create_exclusive`", and without `compare_and_delete` the delete cannot be tied to the stale value: a second
taker can delete the lease a first taker has just created, and both then hold it. `create_exclusive` arbitrates
the create, not the delete before it. A re-read before the delete narrows the window and does not close it.
**Decision proposed: `lock.lease` and `lock.fencing` stay refused on blob stores** (typed `Unsupported`, as now).
A program that needs a cross-process lease runs it on SQLite, PostgreSQL, MySQL or MariaDB (all on), or uses
`lock.process` for one host.

## Expiry: a prefix sweep, not `kv.purge_expired`

`kv.purge_expired` would list keys and delete the expired ones. A delete cannot be tied to the value it saw,
so it can remove a value written again after the check. **Not emulated.** What is safe is the retention policy
the consumer already owns (PM 2026-10-17, relational `sweep`): the **key carries its time**
(`events/2026-11-22/...`) and nothing ever writes a key again once its period has passed, so
`purge_prefix_older_than(prefix, cutoff)` lists under the prefix and deletes keys whose embedded period is
before the cutoff. It is correct only under that key discipline, so its documentation says that first, and it
takes the key-to-time parse from the caller. (`kv.ttl` stays what it is: reads treat an expired key as absent;
space is reclaimed by this sweep.)

## Migrations: a ledger of exclusive creates

For a store of files or keys (a directory layout, an index file format) a migration ledger needs no new
primitive: `migrations/<id>.claim` is made with `create_exclusive` (one winner per step), the step runs, then
`migrations/<id>.done` is written. A second migrator that loses the claim waits or exits. **Limit:** a crash
between claim and done leaves a claimed step that no one will take, because taking it over has the same
delete-race as a lease; the operator removes the `.claim` after checking the store, and the ledger says so by
name (`migrations: step <id> claimed at <time> by <host/pid>, not done`). Steps must be idempotent, so a repeated
step is harmless. This is concurrency-safe for exactly the claim, and honest about the crash.

## To build, in order (each is its own slice, with its known-bad control and a mutation check)

1. `purge_prefix_older_than` (blob and kv-on-blob): control = a key whose period has not passed must survive;
   mutation = a cutoff off by one period must make the test fail.
2. The blob migration ledger: control = two migrators racing one step run it once; crash test = a killed child
   leaves `.claim` without `.done` and the next run reports it by name instead of re-running.
3. Leases on blob: **not built**; revisit only if OpenDAL's `fs` service gains an atomic compare-and-delete
   (a conditional delete) that a matrix row can certify.

## Not covered

Object stores (S3, GCS): they have conditional writes (`if-match`, `if-none-match`), which would change the
lease answer, but no driver here exposes them (row 43 second part). Redis is held (row 32).
