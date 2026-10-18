# Verifying `Machine` durability (power loss): a design note

Written 2026-10-17 by the PersistAnt lane at main 0.7.8. **A note, not a plan of record**: it ends in a
decision for the PM and CireSnave. No code accompanies it. Facts about this PC were read on 2026-10-17
(Windows 11 Pro, 63 GB, `HypervisorPresent=True`, services `vmms` and `vmcompute` running, WSL2
distros `Alpine`, `lane-spike`, `docker-desktop`); I did not run a VM or elevate.

> **Status 2026-10-18: decided and being built.** CireSnave ruled board 229 for Hyper-V (A) and approved the
> VM setup. The harness is `scripts/machine-durability/` (README there) with the guest side in
> `crates/persistant-sqlx/examples/machine_vm.rs`. Deviations from section 3 below: the host reads the
> acknowledgements over SSH rather than the serial port (no pipe permission to depend on), and each run
> copies a base disk instead of restoring a checkpoint. Nothing is marked `Machine`-verified until a run
> on the runner has passed with its known-bad control losing.

## 1. What is being proved

`kv.durability.process` (0.5.8) kills the writer after acknowledged writes and reopens the store. A
process kill leaves the OS page cache intact, so it cannot show that an acknowledged write reached
stable storage. `Machine` means: **an acknowledged write survives the loss of the whole OS** (power
cut, kernel panic). The test shape is the same, one level down: write on a guest, acknowledge, cut the
guest's power, boot, read back.

The control must be real: a store that acknowledges before it flushes (SQLite `synchronous=OFF`, a
file written with no `fsync`) must **lose** acknowledged writes, or the harness proves nothing. To make
the control deterministic, cut power within milliseconds of the acknowledgement and raise the guest's
`vm.dirty_expire_centisecs` / `vm.dirty_writeback_centisecs` so the kernel does not write back first.

## 2. Options on this PC

| Option | Cuts power to the guest? | Cost | Verdict |
|---|---|---|---|
| `docker kill`, `wsl --terminate` | No: process only | none | Already covered by `Process`. |
| `wsl --shutdown` | No: a graceful shutdown that syncs | none | Invalid; would pass the known-bad control. |
| **Hyper-V Gen2 Linux guest, `Stop-VM -TurnOff`** | Yes: guest memory is discarded, no sync | one-time admin, a guest image (~4 GB disk, 2-4 GB RAM), a PowerShell driver | **Recommended.** `vmms` is already running. |
| QEMU (WHPX) and `kill -9` of the QEMU process | Yes, same as Hyper-V | QEMU not installed; installing and trusting a binary; slower without KVM-class acceleration | Fallback if Hyper-V is refused. |
| Linux `dm-flakey` / `dm-log-writes` in a guest | Simulates a lost write cache per block | needs a guest kernel with the modules; I have not checked the stock WSL2 kernel | Useful later for *torn* writes; not a replacement. |
| A real machine on a switched outlet / hard reset | Yes, **physically** | spare hardware, a smart plug, a runner on it | The only way to cover the disk's own cache. Not now. |

## 3. Recommended harness

- A Gen2 Ubuntu guest on this PC, a checkpoint taken after setup so every run starts clean.
- A small `persistant` example binary in the guest (the existing `crash` writer, which already
  acknowledges and reports) writes N keys, prints each key after its acknowledgement over the guest's
  serial/KVP channel or SSH, and is never told to stop.
- The driver (PowerShell, on the self-hosted runner) waits for the Nth acknowledgement, runs
  `Stop-VM -TurnOff -Force`, starts the VM, and runs a reader that checks **every acknowledged key is
  present with its value**. Repeat M cycles (the failure is probabilistic).
- Targets in the guest: SQLite file, OpenDAL fs, and PostgreSQL and MySQL installed in the guest.
  Capability id `kv.durability.machine`, with the known-bad control run through the same cycle.
- It is a test, so it runs on the self-hosted runner under the conditions CireSnave set on
  2026-10-16 (`CIRESNAVE-EXPECTATIONS.md` 5.1g/5.1h): never `#[ignore]`, red when a backend regresses.
- Cost per run: boot plus a few seconds per cycle; with M=20 cycles per backend, about 10-15 minutes
  for the four targets. Not for every PR; on `main` and nightly.

## 4. What it proves, and what stays UNVERIFIED

**Proves:** on a Linux guest on Hyper-V, with its virtual disk, an acknowledged write survives the
loss of the guest, for each tested backend and filesystem (ext4). It also proves the backend does not
acknowledge before `fsync`, because the control would otherwise pass.

**Stays UNVERIFIED, and the matrix must say so:**

1. **The physical disk's volatile cache and the host's power.** The host stays up, so Hyper-V's own
   buffering survives the guest cut. Whether a flush reached silicon is a property of the drive.
2. **Other operating systems and filesystems.** NTFS and Windows `FlushFileBuffers` (what a Windows
   build of PersistAnt uses) are not exercised by a Linux guest. A Windows guest is possible
   (Hyper-V runs one) but needs a licence and a bigger image; it is a second step.
3. **Torn writes inside a sector,** which a guest power cut does not produce.
4. **Network filesystems and remote object stores** (OpenDAL services other than `fs`): their
   durability is the provider's and is declared, not tested.

The verdict for a passing backend should therefore be `Exact` for "guest power loss on ext4/Hyper-V"
with those four limits in its `note`, never a bare `durability.machine = on`.

## 5. Decision for the PM and CireSnave

Recommend **Hyper-V**. It needs: (a) the runner's user added to the *Hyper-V Administrators* group once
(an elevation I cannot do from a lane), (b) about 20 GB of disk and 4 GB of RAM while a run is live,
(c) a decision on who builds and keeps the guest image (it must be reproducible from a script, not
hand-made). Board 229 depends on this. If Hyper-V is refused, QEMU is the fallback; a physical
machine is the way to cover what this cannot, and is not proposed now.
