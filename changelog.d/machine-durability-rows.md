kind: Added: matrix/opendal-machine.tsv carries its first blob.durable rows (opendal-fs, opendal-fs-atomic)
---
`matrix/opendal-machine.tsv` now holds the `blob.durable` rows from the `machine-durability-matrix` artifact of push run 45302864576 (main `e1ca6b9`, OpenDAL 0.59.5): `opendal-fs` and `opendal-fs-atomic` are `yes`, 3 of 3 power-cut cycles held with 300 of 300 keys acked, controls as required. Scope as the file states: a Hyper-V guest power cut on ext4. UNVERIFIED: the physical disk's volatile cache and host power loss, NTFS and other filesystems, torn sector writes, remote object stores. The file is copied from the artifact, not edited. No code change.

The portfolio workflow pin in `ci.yml` moves to coderipper `dc45e36` (0.47.21).
