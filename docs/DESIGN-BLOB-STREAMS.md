# Design note: ranged and streamed blob I/O

Status: **proposal, no code** (ROADMAP "Portfolio adoption" row 4; PM [TASK] 2026-10-29: design first, it blocks mlmf). Read at `b629c5c` (0.12.3): `crates/persistant-blob/src/backend.rs` (the `BlobBackend` trait), `crates/persistant-opendal/src/lib.rs`, `docs/CONSUMERS.md` (mlmf row), `docs/DESIGN-KINDS.md` section 3.
What was **not** read: mlmf's source. Its needs below are from `CONSUMERS.md` (read at mlmf `cb3168db`) and must be confirmed by the mlmf lane (section 6).

## 1. What is true today

- `BlobBackend` is whole-value: `read(key) -> Option<Vec<u8>>`, `write(key, Vec<u8>)`, `replace_atomic`, `append`, `create_exclusive`, plus `stat` (length only), `list`, `delete`. A value is one `Vec<u8>` in memory on both sides of the call.
- A multi-gigabyte value cannot go through it without holding all of it in RAM, and a partial read of a big file means reading all of it.
- `Entry` carries `key` and `len`, so a caller can learn the size without reading.
- mlmf (`CONSUMERS.md`): model files are read through `memmap2` (a mapped, random-access read of files of many gigabytes); checkpoints are written to a temporary file then `fs::rename`d; the hub download cache writes a `.part` file then renames. `CONSUMERS.md` says mlmf "waits on a streaming blob API".

## 2. What is asked for

Three separate things hide in "streaming". They have different guarantees and different backends, so they are three capabilities, each on only where a test passed.

| Id | Meaning | Primary consumer |
|---|---|---|
| `blob.read_range` | the bytes `[offset, offset+len)` of a value without reading the rest | model files (random access), resuming a download |
| `blob.write_stream` | a value written in chunks through a handle; **nothing is visible at `key` until `finish`** | checkpoints, downloads |
| `blob.write_stream.atomic` | as above, and a concurrent reader sees the whole old value or the whole new one, never a mix (the streamed `blob.replace_atomic`) | checkpoints replacing the previous one |

Not offered, with the reason: **memory mapping** (it needs a local file; see decision 1), **`write_at`** (S3 and most object stores cannot overwrite a range; a capability only `fs` has is a trap), and a **streamed read handle** (it is `read_range` in a loop; the crate offers the loop as a helper, not as a trait method or a capability, so there is one thing to certify).

## 3. Design

### 3.1 Trait additions (all with defaults that return `Unsupported`, so no existing backend breaks)

```rust
/// Bytes [offset, offset+len) of `key`; `None` if absent. A range that runs past the end returns
/// the bytes that exist (possibly none); `len == 0` returns an empty vector for a present key.
fn read_range(&self, key: &str, offset: u64, len: u64)
    -> impl Future<Output = Result<Option<Vec<u8>>, BlobError>> + Send;

/// Begin a streamed write of `key`. Nothing is visible at `key` until `finish`; dropping the sink
/// or `abort` leaves `key` as it was and removes what was staged.
fn write_stream(&self, key: &str)
    -> impl Future<Output = Result<Box<dyn BlobSink>, BlobError>> + Send;

/// As `write_stream`, and `finish` replaces atomically (`blob.write_stream.atomic`).
fn write_stream_atomic(&self, key: &str)
    -> impl Future<Output = Result<Box<dyn BlobSink>, BlobError>> + Send;

pub trait BlobSink: Send {
    fn write(&mut self, chunk: &[u8]) -> BoxFuture<'_, Result<(), BlobError>>;
    fn finish(self: Box<Self>) -> BoxFuture<'static, Result<(), BlobError>>;
    fn abort(self: Box<Self>) -> BoxFuture<'static, Result<(), BlobError>>;
}
```

The sink is a boxed trait object because the trait uses `impl Future` returns (no associated types today) and the sink's type differs per backend; one allocation per stream, not per chunk. `write` takes a borrowed slice so a caller reuses one buffer; the backend copies what it must keep.

Why two write methods and not a flag: `write_stream` is the weaker promise (a backend that can stream but cannot make `finish` replace the value as a whole must not be asked for the second, and the matrix says which it did); on `s3` a multipart upload is invisible until completed and its completion replaces the object whole, so `s3` is a candidate for both (3.3), decided by the tests, not by this note.

### 3.2 Helpers (in `persistant-blob`, not in the trait)

- `read_chunks(backend, key, chunk)`: a loop over `read_range` with a default 8 MiB chunk. It reads the entry's version tag (3.7) before the first range and after the last and returns `BlobError::Changed` if they differ; on a backend with no tag it can detect only a length change and says so in its doc, so a same-length replacement during the loop is **not** detected there. Memory bound: one chunk.
- `copy_in(sink, reader, chunk)` for a `Read`/`AsyncRead` source. Memory bound: one chunk.
- A sink's abort-on-drop cannot be async. `Drop` marks the sink aborted and queues its staging for removal; the backend removes queued staging **before it begins the next stream on the same store** and at open (3.8), so a dropped sink never outlives the next `write_stream` call on that store. Callers who care call `abort`.

### 3.3 Backends

| Backend | `read_range` | `write_stream` | `.atomic` | Note |
|---|---|---|---|---|
| `memory` | slice of the stored vector | buffer in the sink, insert on `finish` | yes (one insert under the lock) | the reference for the contract test |
| `opendal-fs` | OpenDAL ranged read | OpenDAL writer into the scratch directory | yes **only with `atomic_write_dir` set**, as `replace_atomic` today | to confirm in work order 2 that the writer's close is a rename; if not, stage by hand in the scratch dir and rename |
| `opendal-s3` (not a PersistAnt backend today) | ranged GET | multipart upload | **candidate yes**: completing a multipart upload replaces the object as a whole, which meets the whole-old-or-whole-new contract; on only if test 5 passes against it | listed so the trait is not fs-shaped |

I have not run OpenDAL's reader/writer here; the table is the plan, and every cell is "on" only after its test.

### 3.4 Tests (the contract, with a known-bad control for each)

1. **Range equals slice:** write a 3 MiB value of a position-dependent pattern; `read_range` at offsets 0, 1, mid, `len-1`, past the end and `len` 0 equals the same slice of `read`. Known-bad: a backend whose range ignores the offset must fail the mid-offset case.
2. **Streamed write equals whole write:** chunks of 1, 1000 and 1 MiB produce a value equal to `write` of the concatenation. Before `finish`, for an **existing** key `read` returns bytes equal to the complete old value (a length check is not enough: a same-length in-place overwrite must fail), and for a new key `read` is `None`. Known-bad control: a test-only backend whose sink writes through to `key` as chunks arrive must fail both assertions.
3. **Abort and drop leave the old value:** old value present; write a stream; `abort` and (separately) drop; `read` returns the old value; a second `write_stream` on the same open store then succeeds and, before it finishes, the store holds no staging from the dropped sink (the backend removes queued staging before beginning it, 3.2). Known-bad control: a sink whose `abort` and drop leave the staging file in place must fail the staging assertion.
4. **Crash, two kill points.** (a) A child streaming into `key` is killed before `finish`; the parent finds the old value (or none) and, after reopen, no staging. (b) For `write_stream_atomic`, a child is killed **during** `finish` (the test-only failpoint pauses inside the publish step and the parent kills it there); the parent must read the complete old value or the complete new value, never a mix, and a later `write_stream_atomic` on the key works. Known-bad controls: a test-only **in-place** writer (it opens `key` itself and writes straight into it, bypassing the scratch directory and rename) must be left with a partial value by (a) and (b); the ordinary backend `write` is **not** used as the control, because with `atomic_write_dir` set it also goes through scratch-and-rename and would leave the old value.
5. **Concurrent reader** (`.atomic` only): the existing `blob.replace_atomic.concurrent_read` harness against `write_stream_atomic`. Known-bad control: the test-only in-place writer as `write_stream_atomic` must be caught showing a torn read.
6. **Bounded memory:** a 256 MiB value copied through `copy_in` with an 8 MiB chunk stays under a stated peak (measured by the test's own allocator count, not by host RAM, which varies). Known-bad control: a `copy_in` variant that buffers the whole value must exceed the peak.
7. **Through the wrappers:** tests 1 to 3 and 5 run against `Traced` and `Instrumented` around `memory` and give the same results as unwrapped (3.5). Known-bad control: a wrapper that does not forward must fail with `Unsupported`.

### 3.5 Registry and blocking mirror

`AnyBlob` forwards the three, and **each forwarded operation first calls the existing reserved-key check (`check(key)`) exactly as the current nine do**, so `read_range` and the streams cannot reach `kv`, `kvx` or anything under `kv/` or `kvx/` (test: the reserved keys are refused on `read_range`, `write_stream` and `write_stream_atomic`, with lookalike names accepted as the control, as in 0.11.14). `blocking::registry::BlobHandle` gets `read_range` and a blocking sink (`write`, `finish`, `abort`) on the same worker, so one worker serves every kind as today. **`Traced` and `Instrumented` forward the new operations to the inner backend** (otherwise they inherit the `Unsupported` defaults and `stats::measure` cannot certify the capabilities through them); a Traced span per ranged read and per sink open, write, finish, abort uses the shared tracing names. A need naming `blob.read_range` etc. is met only by providers whose matrix rows say yes, through `capant::evaluate` as for every other id (a missing row is `unverified`, not a closest match).

### 3.6 Vocabulary and matrix

Three new leaves, append-only: `blob.read_range`, `blob.write_stream`, `blob.write_stream.atomic`. Rows are written by `conformance::certify` and regenerated by CI; the matrix job turns red once when the new rows first appear, and the rows are committed from the CI artifact (rows carry the crate version's first two numbers, so a 0.12.x release keeps its existing rows). The PM rules these additive (section 8), so they ship as 0.12.x; the PM allocates each number.

### 3.7 Version tags for multi-range reads

`Entry` gains `tag: Option<String>`, a backend's version token for the stored value (OpenDAL's etag or last-modified plus length where it has them; a counter on `memory`). `read_range_if(key, offset, len, tag)` returns `BlobError::Changed` when the stored tag differs. A backend with no tag returns `tag: None` and says so in its matrix row (`blob.read_range.consistent` off); callers who need one version across ranges then hold their own lock. A length comparison is **not** offered as protection.

### 3.8 Staging ownership across processes

Staging names carry an owner id (process id plus a random token) and the sink holds an advisory file lock on its staging file for as long as it lives. Cleanup (at open and before a new stream) removes only staging whose lock it can take, that is, staging whose owner is gone; staging held by a live sink in another process is left alone. Test: store opened in a second process while a sink in the first is mid-stream; the first sink's `finish` still succeeds with the whole value. Known-bad control: a cleanup that removes every staging file fails it.

## 4. Work orders (each a PR; none starts before this note merges)

1. Vocabulary ids, `read_range`, `read_range_if` with `Entry.tag` (3.7) and `BlobSink` in the trait with `Unsupported` defaults, `memory` implementation, the contract tests (1 to 3, 5 and 7) with their known-bad controls, the chunk helpers, and `Traced` and `Instrumented` forwarding the new operations.
2. `opendal-fs`: `read_range`, `write_stream`, `.atomic` with the scratch directory; confirm OpenDAL's writer semantics first and record what was found.
3. Crash tests (4a and 4b, with the in-place control), the cross-process staging test (3.8), bounded-memory test (6); matrix rows; `docs/DESIGN-KINDS.md` section 3 table gains the three ids.
4. Registry (`AnyBlob`, each operation through `check(key)`; reserved-key test) and blocking mirror; `docs/REGISTRY-FOR-STORE-USERS.md`; an example `examples/registry_blob_stream.rs` run by CI.
5. Patent re-sweep for the new family (see section 7) before the publish.

## 5. Caveats stated now

- `Drop` cannot await; its cleanup is queued and runs before the next stream on the same store and at open (3.2), not at the moment of the drop. Callers who care call `abort`.
- `read_range` on a value being replaced concurrently sees one version per call (the backend's own guarantee, stated per backend). Across calls, only the version tag (3.7) detects a replacement, and only on a backend that has one.
- Nothing here makes `blob.durable` true for a streamed write; durability stays its own capability with its own VM test.

## 6. Questions for the mlmf lane (ask before work order 1)

1. Is mmap of the model file required, or are ranged reads (offset, length) enough? If mmap is required, decision 1 applies.
2. Typical sizes and chunk sizes of a checkpoint write and a model read, to set the default chunk and the bounded-memory test.
3. Does the download cache resume a `.part` file (needs `read_range` of the partial and an append, which `blob.append` gives on `fs`), or restart?
4. Which backend does mlmf run on (`fs` only, or S3 for checkpoints)?

### 6.1 Answers (mlmf lane via the PM, 2026-10-29, evidence from mlmf origin/main `4c6d47b`; not re-read here)

1. **mmap is not required.** mlmf's seam is `ByteSource::as_bytes()` (a whole slice) plus `RangedSource::read_range` (no caller outside its own implementation); parsers take whole slices, so range-only parsing is not built yet. `blob.local_path` is declined.
2. **No sizes are encoded.** Writes are one whole `Vec<u8>` today (no chunking); reads are whole-file. The default chunk (8 MiB) and the bounded-memory test stand on this note's own numbers, not mlmf's.
3. **No resume.** `HubSource::download` does `File::create(.part)`, `io::copy`, `rename` (source-hub `lib.rs:222-250`); a retry restarts at 0 with no Range header. So the resume use of `read_range` and `append` in section 2 is not demanded.
4. **`fs` only**; no S3 or `object_store` dependency anywhere. So work orders 1 to 3 (memory, then `opendal-fs`, then the crash test and rows) are what mlmf needs; S3 qualification is not demanded by mlmf and stays a design property of the trait, not a work order.

**Work order 1 may start once this note merges.**

## 7. Patent note

A new capability family, so `docs/PATENT-SWEEP.md` asks for a re-sweep before it ships. Ideas to sweep (queries not yet run): ranged read of a stored value through a uniform storage trait; staged streamed write invisible until a finish call, with abort-on-drop and staging cleanup at next open; a streamed-write contract tested with a killed writer and a known-bad control. Standing conditions in `ROADMAP.md` are untouched.

## 8. Decisions (PM [FIX] 2026-10-29)

1. **mmap: declined by default.** The PM asks the mlmf lane section 6 first. `blob.local_path` on `fs` is offered only if mlmf answers that mmap is required, and then as its own work order.
2. **Both write methods stay:** `write_stream` and `write_stream_atomic` (so `s3` can qualify).
3. **Version:** the additions have `Unsupported` defaults and new matrix ids, so they are additive and the work orders are 0.12.x; 0.13.0 only if a work order breaks something. The PM allocates each number at the gate.
