kind: `KvOnRelational::delete_erased` (row 38, slice 3b)
---
`KvOnRelational` implements `KvBackend::delete_erased` through `RelationalBackend::execute_erased`: erased on SQLite (`secure_delete` plus a truncating WAL checkpoint, see 0.18.6), `Unsupported` on PostgreSQL and MySQL/MariaDB, which have no such statement (a deleted tuple stays until vacuum / purge, and WAL and backups keep it).

Verified by scanning the raw database file, `-wal` and `-journal` for a canary value after the delete, on `open` and `open_durable`: plain `kv.delete` leaves it (the control), `kv.delete_erased` does not; routing it through plain `execute` fails the test. **Not covered:** a value an earlier plain `put` overwrote (old bytes may sit in free space inside a page), and anything a file system or backup keeps.
