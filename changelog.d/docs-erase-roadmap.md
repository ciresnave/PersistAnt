kind: docs: README limits for atomic batches and erased delete; ROADMAP rows 38, 39, 40
---
README "Limits" now says where `kv.batch_atomic` is available (SQLite, PostgreSQL, MySQL and MariaDB; not a blob store through `KvOnBlob`, so the matrix has no rows for memory or `fs`) and what `blob.delete_erased` / `kv.delete_erased` promise and do not (contents overwritten, not gone from the disk; per-backend state). ROADMAP row 38 records what is built, row 39 stays scheduled, row 40 records `Endpoint::fits` as done. Documentation only.
