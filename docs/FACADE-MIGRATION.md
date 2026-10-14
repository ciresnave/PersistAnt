# Moving the `persistant` facade onto the new crates: a design note

Written 2026-10-14 by the PersistAnt lane at main 0.4.25. **A note, not a plan of record**: it ends in
a decision (D7) for the PM and CireSnave. No code accompanies it.

## 1. What `persistant` is today

One crate on OpenDAL: `Store` (async) and `persistant::blocking::Store`, `Config` (`Memory`, `Fs`,
`Faulty`, `Masked`), `Need`/`Needs`, `Entry`, `Page`, `Record` (versioned JSON envelope),
`Replace` (piecewise write), `Faults`, and an `Error` enum. It is published (0.4.23 on crates.io) and
**two consumers depend on it**: lightbulb (`persistant = "0.4.1"`, the blocking `Store`, needs Read,
Write, Delete, AtomicReplace: `src/cache/tiered_storage.rs:134-140` @ origin/main, read 2026-10-14)
and agentlife (`persistant = "0.4.4"`, two `persistant::` use sites). I read only those lines; other
use sites, and whether either uses `Record`, are **UNVERIFIED**.

## 2. What the new crates have and do not have

| `persistant` item | New equivalent | Gap |
|---|---|---|
| `Store::read/replace/delete/stat/list` | `BlobBackend::read/write/delete/stat/list` | `list` takes `NonZeroUsize` and returns `modified`; `BlobBackend::list` takes `usize` and `Entry` has no `modified` |
| `create_if_absent` | `BlobBackend::create_exclusive` | none (same refusal on fs with `atomic_write_dir`) |
| `replace` with crash safety | `BlobBackend::replace_atomic` | **the leaf `blob.replace_atomic` (concurrent reader *and* crash) has no crash test, so no row says it is on** (§3) |
| `Need`/`Needs`, `Error::Refused` at open | `persistant_core::Matrix` lookup | needs are *claimed* from OpenDAL's declared capability struct today; the new gate reads *tested* rows |
| `Record` (schema versions, upgrade) | none | not ported; a codec concern, not a backend one |
| `begin_replace` / `Replace` (piecewise write) | none | `BlobBackend` has whole-value `write` only |
| `Faults`, `Masked` test fakes | `MemoryBlob` defects | different mechanism (fault at the Nth write vs a named defect); not equivalent |
| `blocking::Store` | none | no blocking facade over the new traits |
| `Need::MaxValueSize` | `blob.max_value_size` leaf, no test | untested, so off |

## 3. The one finding that decides the order: AtomicReplace needs a crash test

`Need::AtomicReplace` promises "old or new, never a mix, **even across an interrupted write**".
The new vocabulary splits that promise: `blob.replace_atomic.concurrent_read` is certified on `fs`
with `atomic_write_dir` and on `memory`; the crash half (`blob.replace_atomic`) has no test.
If `Store::open` were gated on the matrix today, **every consumer's `AtomicReplace` would be
refused**, correctly: nobody has tested the crash half. The crash test is feasible with the same
technique as `ProcessLock`'s: a child process replaces a large value in a loop and is killed hard at
an arbitrary point, and the parent reads the key and requires the whole old or the whole new value;
the known-bad control is a plain in-place write, which a kill leaves truncated. That test is the
prerequisite for any matrix-gated `AtomicReplace`, and it is small.

## 4. Options

**A. Additive: keep `persistant` as it is and re-export the new kinds behind features.** `persistant`
gains `kv`, `relational`, `core` modules (feature-gated, off by default). No behaviour change; no
consumer moves; 0.4.x. The facade is then literally "re-exports; features pick kinds and drivers"
(DESIGN-KINDS §3) without touching `Store`. Cost: two blob code paths exist side by side (the
original OpenDAL `Store` and `persistant-opendal`), which drift unless one is deleted.

**B. Reimplement `Store` over `BlobBackend` + `persistant-opendal`.** One code path, and `Needs`
become matrix lookups, so a refusal is a tested fact. Costs: the gaps in §2 (piecewise `Replace`,
`modified`, `Record`, `blocking`, `Faults`) must be ported or dropped; `AtomicReplace` is refused
until §3 lands; the observable behaviour of `Store::open` changes for consumers whose backend is
not in the recorded matrix (an uncertified OpenDAL version is *off*, not guessed). That is a
**breaking** change: pre-1.0 it is the second number, **0.5.0**, and lightbulb and agentlife take it
deliberately.

**C. Deprecate `Store` and point consumers at the kind crates directly.** The cleanest end state
(no facade logic at all) and the largest consumer change. Only after B has run in anger.

## 5. Recommendation

1. **Now, A.** It is additive, costs nothing to consumers, and makes the new crates reachable from
   the one name consumers already depend on.
2. **Write the crash test (§3)** as the next certification slice; it is a prerequisite for B and it
   is independently valuable (it is the only claim README makes that no automated test yet checks
   the way a crash does: the existing fault fake simulates failure, it does not kill a process).
3. **B only as 0.5.0, after §3**, with the §2 gaps each answered explicitly: `Record` and `blocking`
   move into the facade (they are not backend concerns), `Replace` is either added to `BlobBackend`
   with a test or dropped with a ruling, `Faults`/`Masked` are replaced by defect backends.
4. **C is not proposed yet.**

## 6. Decision for the PM and CireSnave

**D7. Facade strategy.** A now (recommended); A then B as 0.5.0 after the crash test; or leave
`persistant` as it is and let consumers depend on the kind crates directly. Whatever is chosen,
this lane does not move lightbulb or agentlife: each consumer's lane migrates itself and takes
the 0.5.0 deliberately.

## 7. UNVERIFIED

- Every `persistant::` use site in lightbulb and agentlife beyond the lines named in §1; whether
  either uses `Record`, `Replace` or `Faults`.
- That a kill-based crash test is stable on Windows as well as Linux (the `ProcessLock` test is).
- Whether a recorded matrix can name every OpenDAL version a consumer might resolve; today it names
  the one the crate builds against, so a consumer on another resolves to "off".
