# PersistAnt roadmap

Priorities live in the portfolio roadmap; this file holds the rows this repo owns. Design: `docs/DESIGN-KINDS.md`.

## Backend benchmarking and ranking

Approved by CireSnave 2026-10-16: *"I agree with your thinking about PersistAnt's benchmarking of backends.  Get it on the correct roadmaps and lets get it built."*

One benchmark per benchmarkable capability, so PersistAnt can rank backends and a consumer can ask for
the best backend for a capability. Four stages, each its own PR, in order:

1. **Capability contract suite, per kind (kv first).** Pass/fail only. A backend that fails a contract
   is **excluded**, never ranked slower. The model is Synapse's `tests/mailbox_suite`. Motive: Synapse's
   spike found `run_all` passes `MemoryKv` and fails `KvOnBlob` (no `batch`; 0 versus 74/100 duplicate
   sequence numbers with and without `Require`).
2. **Precision and per-operation statistics.** Errors, ordering and durability checks, per operation.
3. **Speed.** The environment is recorded with every result (machine, backend version, date) together
   with a re-run command. No constant rankings.
4. **`best backend for X` API.** Returns a **ranked list with the evidence**. The consumer states the
   capabilities it needs and a minimum durability class; the choice is explicit and logged, never silent.

**Status (main = 0.5.6, 2026-10-16).** All four stages are done for the **kv** kind:
`contract::check` (#39, with the Synapse duplicate-sequence workload as a leaf, #40), `stats::measure`
(#43), `speed` with data files in `bench/` (#46), `choose::choose` (#47). Stage 2 also exists for
blob (#45). Not done, in this order (queued by the PM 2026-10-16):

1. **Durability verification.** `choose` ranks against a durability class that is only *declared*
   (`declared_durability`, each entry with its basis). Verify what can be verified in CI: the
   `Process` class by killing a writer process after acknowledged writes and reading them back after
   restart (the `blob.replace_atomic` crash test is the model). The `Machine` class (power loss, an
   OS crash) needs a VM hard reset and stays **declared** until a harness for that exists.
2. **`choose` for the blob kind**, then **the relational kind**. The relational kind has no leaves of
   its own yet (migrations are tested, not certified): *nothing to measure yet*, owner the PersistAnt
   lane, so its leaves come first.
3. **Synapse `tests/mailbox_suite` as a stage-1 consumer check**: the Synapse lane runs `run_all` on
   each eligible backend (Postgres, SQLite, MySQL) and reports which leaves it adds.
4. Not covered by any stage yet: concurrency-aware speed, value sizes other than 256 bytes, ranking
   by more than one workload.

Rules: benchmarks are tests. They run in CI or on the self-hosted runner, fail loudly on regression, and
use no `#[ignore]` and no `allow(dead_code)`. Detector: the contract suite goes red for a backend that
regresses.

## SQLite and MySQL backends (through UniSQL)

`KvOnRelational` written once in PostgreSQL SQL and translated by UniSQL (`DESIGN-KINDS.md` §5).
Depends on UniSQL 0.24.4 or later from crates.io.
