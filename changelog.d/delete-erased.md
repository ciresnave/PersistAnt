kind: `BlobBackend::delete_erased` (row 38, first slice)
---
New default method `BlobBackend::delete_erased(key)` (capability `blob.delete_erased`): overwrite the stored bytes, then remove the key. The default returns `Unsupported`, so no existing backend changes. `OpendalBlob` on `fs` implements it (zero-fill in place, `sync_all`, unlink); the memory service stays `Unsupported`. `Instrumented` and `Traced` forward it. The promise is "contents overwritten before the name was removed", not "gone from the disk": journaling and copy-on-write file systems, SSD wear levelling, snapshots and backups can keep copies.

Verified by reading the file back through a second hard link to the same inode: plain `delete` leaves the canary readable there (the known-bad control), `delete_erased` leaves zeros; with the overwrite removed the test fails. Not yet done for row 38: `kv.delete_erased`, SQLite (`secure_delete`, WAL checkpoint), PostgreSQL / MySQL, and the matrix row.
