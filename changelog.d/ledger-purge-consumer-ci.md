kind: Added: `KvOnBlob::purge_prefix_older_than`; Ledger killed-process test; consumer_ci on PostgreSQL in CI
---
`KvOnBlob::purge_prefix_older_than(prefix, cutoff, period_of)` (ROADMAP row 19): the blob prefix sweep of 0.18.34, with the period read from the **kv key** (raw and hex layouts), keys that are not this store's records and keys `period_of` cannot read kept. Same key discipline and limit as the blob function (a key names its period and is never written again after it); it is not `kv.purge_expired`. Tests on both layouts with the at-cutoff control; mutation (pass the blob key to `period_of`) fails both.

`persistant_blob::ledger::Ledger` gets its killed-process test (`persistant-opendal/tests/ledger_killed.rs`): a child process killed inside a step leaves `.claim` without `.done`, the next run returns `Claimed("<when> <who>")` and does not run the step, and only after the operator removes the claim does it run; the control, the same child left to finish, records the step done. Test only, no behaviour change.

The Relational (PostgreSQL), MariaDB and MySQL CI jobs run the `consumer_ci` example on its service container (ROADMAP row 18): it must pass for `kv.get,kv.put,kv.put_if_absent,kv.ttl`, and the known-bad control (a leaf id that does not exist) must exit 2. Verified on a local PostgreSQL 16 first: exit 0 and exit 2. `consumer_ci` takes a `mysql://` URL (build with `--features mysql`) for MySQL and MariaDB.

`blob.durable` was measured twice on the Hyper-V guest (runs 45091758886 and, scheduled, 45181235222): `opendal-fs` and `opendal-fs-atomic` HELD 3 of 3 cycles each time, controls as required (`std-synced` HELD, `std-unsynced` LOST). No matrix row or capability change yet: a VM-measured row has no generator in the matrix pipeline.
