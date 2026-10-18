# Machine durability test (power loss)

Proves `Machine`-class durability for the cases below, or shows that a backend loses acknowledged
writes. Design and the decision record: `docs/DESIGN-DURABILITY-MACHINE.md` (Hyper-V approved by
CireSnave 2026-10-18, board 229: A).

| File | What it is |
|---|---|
| `crates/persistant-sqlx/examples/machine_vm.rs` | runs **in the guest**: `write` (prints `acked N` after each put returns), `check` (keys 0..N present), `enospc` (write until a put fails) |
| `guest-user-data.yaml` | cloud-init for the guest: PostgreSQL, MySQL, a test user, sysctls that keep unflushed data in cache, a fillable 8 MB ext4 volume |
| `New-SeedIso.ps1` | writes the cloud-init ISO with the IMAPI2 COM API (nothing to install) |
| `build-image.ps1` | **once, by a person**: pinned Ubuntu 24.04 cloud image (SHA-256 checked) -> VHDX (qemu-img in Docker) -> provisioned and powered off |
| `run.ps1` | the test: copy the base disk, boot a Gen2 VM, per cycle write over SSH, `Stop-VM -TurnOff` the moment N acknowledgements have been seen, boot, check |
| `.github/workflows/machine-durability.yml` | builds the guest binary on a hosted runner, runs `run.ps1` on the self-hosted runner (push to main, nightly, dispatch; never on a pull request) |

## What is checked, and the control

* **Honest backends must HOLD:** SQLite WAL `synchronous=FULL`, PostgreSQL and MySQL at their defaults,
  on ext4 in the guest. Every key the host saw acknowledged must be present with its value after the cut.
* **Known-bad control must LOSE:** SQLite `synchronous=OFF` with the guest's dirty-page timers raised to
  an hour. If it loses nothing in any of its cycles the run fails: a harness that cannot show a loss
  proves nothing, and no result from it counts.
* **Disk full must fail loud:** SQLite on an 8 MB volume is written until a put fails (exit 3); then every
  key acknowledged before the failure must still be there. A run in which nothing fails (exit 4) fails.
  PostgreSQL and MySQL on the small volume are not yet covered (their data directory has to move).

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
