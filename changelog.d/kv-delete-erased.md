kind: `KvBackend::delete_erased` (row 38, slice 2)
---
New default method `KvBackend::delete_erased(key)` (capability `kv.delete_erased`): overwrite the bytes that stored the value, then remove the key. Default `Unsupported`, so no existing backend changes. `KvOnBlob` forwards to the blob's `delete_erased`, so it works over `OpendalBlob` on `fs` (zero-fill, sync, unlink) and is `Unsupported` over a blob that lacks it. `Instrumented`, `Traced` and the fault wrapper forward it. Same promise as the blob leaf: contents overwritten, not "gone from the disk".

Verified through a second hard link to the record file: plain `kv.delete` leaves the value readable there (control); `kv.delete_erased` does not. Not yet: SQLite (`secure_delete`, WAL checkpoint), PostgreSQL / MySQL, KvOnRelational, and the matrix rows.
