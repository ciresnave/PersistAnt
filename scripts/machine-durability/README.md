# Machine durability test (power loss)

Proves `Machine`-class durability for the cases below, or shows that a backend loses acknowledged
writes. Design and the decision record: `docs/DESIGN-DURABILITY-MACHINE.md` (Hyper-V approved by
CireSnave 2026-10-18, board 229: A).

| File | What it is |
|---|---|
| `crates/persistant-sqlx/examples/machine_vm.rs` | runs **in the guest**: `write` (prints `acked N` after each put returns), `check` (keys 0..N present), `enospc` (write until a put fails) |
| `pa-small-db.sh` | runs **in the guest**: a small volume over the PostgreSQL or MySQL data directory for the disk-full check (`on`, `free`, `off`) |
| `guest-user-data.yaml` | cloud-init for the guest: PostgreSQL, MySQL, a test user, sysctls that keep unflushed data in cache, a fillable 8 MB ext4 volume |
| `New-SeedIso.ps1` | writes the cloud-init ISO with the IMAPI2 COM API (nothing to install) |
| `build-image.ps1` | **once, by a person**: pinned Ubuntu 24.04 cloud image (SHA-256 checked) -> VHDX (qemu-img in Docker) -> provisioned and powered off |
| `run.ps1` | the test: copy the base disk, boot a Gen2 VM, per cycle write over SSH, `Stop-VM -TurnOff` the moment N acknowledgements have been seen, boot, check |
| `.github/workflows/synapse-power-cut.yml` | the same `run.ps1` driven with Synapse's `mailbox_vm` (dispatch only, a Synapse commit that is on its main) |
| `.github/workflows/machine-durability.yml` | builds the guest binary on a hosted runner, runs `run.ps1` on the self-hosted runner (push to main, nightly, dispatch; never on a pull request) |

## What is checked, and the control

* **Honest backends must HOLD:** SQLite WAL `synchronous=FULL`, PostgreSQL and MySQL at their defaults,
  on ext4 in the guest. Every key the host saw acknowledged must be present with its value after the cut.
* **Known-bad control must LOSE:** SQLite `synchronous=OFF` with the guest's dirty-page timers raised to
  an hour. If it loses nothing in any of its cycles the run fails: a harness that cannot show a loss
  proves nothing, and no result from it counts.
* **Disk full must fail loud:** SQLite on an 8 MB volume is written until a put fails (exit 3); then every
  key acknowledged before the failure must still be there. A run in which nothing fails (exit 4) fails.
  PostgreSQL (80 MB) and MySQL (200 MB) get a fresh ext4 volume mounted **over their own data directory**
  (`pa-small-db.sh`, copied into the guest at run time, so the base image is not rebuilt), a new cluster
  in it, and puts until one fails. A put that gets no answer in 60 s counts as failed and says
  "stalled" (InnoDB waits for space instead of returning an error). The server may then be down or
  waiting, so the operator's step is done before the check: the 8 MB ballast file is deleted, the stopped server is started again, the
  server is awaited, and every key acknowledged before the failure must be present.

## What the disk-full runs found (run 39325665814, main 9a9fad2)

| Backend | Behaviour on a full volume | Acknowledged keys |
|---|---|---|
| SQLite (8 MB) | **fails loud** | 330 kept |
| PostgreSQL (80 MB) | **fails loud**: `could not write to file pg_wal/...: No space left on device`; the server can PANIC and stays down until about 10 MB is freed and it is started again | 21,496, all kept |
| MySQL (200 MB) | **STALLS**: no error, no answer; the binlog write waits (`Disk is full ... Waiting for someone to free space`) | 7,118, all kept once space was freed |

The PostgreSQL log shows `duplicate key value violates unique constraint "persistant_kv_batch_pkey"` each
time a store is opened: `KvOnRelational::open` inserts the batch-lock row `(1)` on every open and treats a
unique violation as "already there" (`crates/persistant-kv/src/on_relational.rs`). Every `machine_vm`
invocation (write, enospc, check) opens the store, so one such line per invocation is expected and harmless.

## What a pass does not show (UNVERIFIED, and must stay so in any matrix row)

The physical disk's volatile cache and host power loss (the host stays up); NTFS and Windows flush
behaviour; torn sector writes; remote object stores (OpenDAL services other than `fs`).

## One-time setup (needs an administrator)

1. `Add-LocalGroupMember -Group 'Hyper-V Administrators' -Member 'TUF-LAPTOP\gh-runner','TUF-LAPTOP\<you>'`,
   then restart the runner service and start a new session (group tokens load at logon).
2. `pwsh scripts/machine-durability/build-image.ps1` (Docker needed, as a user in that group).
3. Give `gh-runner` read access to `C:\ProgramData\persistant-machine-durability` (`base.vhdx`, `id_pa`).
4. Register the repo with the runner (the PM does this, per `CIRESNAVE-EXPECTATIONS.md` 5.1h).

Run by hand: `pwsh scripts/machine-durability/run.ps1 -Binary <machine_vm built on Ubuntu 24.04>`.
