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

Why two write methods and not a flag: `write_stream` is the weaker promise (on `s3` it is a multipart upload, which is invisible until completed but is not "replace atomically" in the sense a reader of the old value sees); a backend that can only do the first must not be asked for the second, and the matrix says which it did.

### 3.2 Helpers (in `persistant-blob`, not in the trait)

- `read_chunks(backend, key, chunk) -> impl Stream`-like loop over `read_range` with a default 8 MiB chunk and `stat` for the end. Memory bound: one chunk.
- `copy_in(sink, reader, chunk)` for a `Read`/`AsyncRead` source. Memory bound: one chunk.
- A sink's abort-on-drop cannot be async; `Drop` marks it aborted and the staged file is removed on the next open of the same store (recorded as a caveat, section 5).

### 3.3 Backends

| Backend | `read_range` | `write_stream` | `.atomic` | Note |
|---|---|---|---|---|
| `memory` | slice of the stored vector | buffer in the sink, insert on `finish` | yes (one insert under the lock) | the reference for the contract test |
| `opendal-fs` | OpenDAL ranged read | OpenDAL writer into the scratch directory | yes **only with `atomic_write_dir` set**, as `replace_atomic` today | to confirm in work order 2 that the writer's close is a rename; if not, stage by hand in the scratch dir and rename |
| `opendal-s3` (not a PersistAnt backend today) | ranged GET | multipart upload | no (a completed multipart upload replaces the object, but the contract test decides) | listed so the trait is not fs-shaped |

I have not run OpenDAL's reader/writer here; the table is the plan, and every cell is "on" only after its test.

### 3.4 Tests (the contract, with a known-bad control for each)

1. **Range equals slice:** write a 3 MiB value of a position-dependent pattern; `read_range` at offsets 0, 1, mid, `len-1`, past the end and `len` 0 equals the same slice of `read`. Known-bad: a backend whose range ignores the offset must fail the mid-offset case.
2. **Streamed write equals whole write:** chunks of 1, 1000 and 1 MiB produce a value equal to `write` of the concatenation; nothing at `key` before `finish` (`stat` is `None`, or the old value stays).
3. **Abort and drop leave the old value:** old value present; write a stream; `abort` and (separately) drop; `read` returns the old value; a second stream then works (no stale staging).
4. **Crash:** a child process streaming into `key` is killed before `finish`; the parent finds the old value (or none) and the staging area is cleaned on reopen. Known-bad control: the same child writing straight to `key` (the ordinary `write`) leaves a partial value, which the test must see.
5. **Concurrent reader** (`.atomic` only): the existing `blob.replace_atomic.concurrent_read` harness against `write_stream_atomic`.
6. **Bounded memory:** a 256 MiB value copied through `copy_in` with an 8 MiB chunk stays under a stated peak (measured by the test's own allocator count, not by host RAM, which varies).

### 3.5 Registry and blocking mirror

`AnyBlob` forwards the three; `blocking::registry::BlobHandle` gets `read_range` and a blocking sink (`write`, `finish`, `abort`) on the same worker, so one worker serves every kind as today. A need naming `blob.read_range` etc. is met only by providers whose matrix rows say yes, through `capant::evaluate` as for every other id (a missing row is `unverified`, not a closest match).

### 3.6 Vocabulary and matrix

Three new leaves, append-only: `blob.read_range`, `blob.write_stream`, `blob.write_stream.atomic`. Rows are written by `conformance::certify` and regenerated by CI; adding rows is a second-number change (rows carry the crate version's first two numbers), so this ships as 0.13.0 or later, which the PM allocates.

## 4. Work orders (each a PR; none starts before this note merges)

1. Vocabulary ids, `read_range` and `BlobSink` in the trait with `Unsupported` defaults, `memory` implementation, the contract tests (1 to 3, 5) with their known-bad controls, the chunk helpers.
2. `opendal-fs`: `read_range`, `write_stream`, `.atomic` with the scratch directory; confirm OpenDAL's writer semantics first and record what was found.
3. Crash test (4) and bounded-memory test (6); matrix rows; `docs/DESIGN-KINDS.md` section 3 table gains the three ids.
4. Registry (`AnyBlob`) and blocking mirror; `docs/REGISTRY-FOR-STORE-USERS.md`; an example `examples/registry_blob_stream.rs` run by CI.
5. Patent re-sweep for the new family (see section 7) before the publish.

## 5. Caveats stated now

- `Drop` cannot await, so a dropped, unfinished sink leaves staging that is removed at the next open of the store, not at once. Callers who care call `abort`.
- `read_range` on a value being replaced concurrently sees one version per call (the backend's own guarantee, stated per backend), not a consistent view across calls. A caller reading a file in ranges while it is replaced must `stat` first and compare `len`, or hold its own lock; the note says so rather than promising a snapshot.
- Nothing here makes `blob.durable` true for a streamed write; durability stays its own capability with its own VM test.

## 6. Questions for the mlmf lane (ask before work order 1)

1. Is mmap of the model file required, or are ranged reads (offset, length) enough? If mmap is required, decision 1 applies.
2. Typical sizes and chunk sizes of a checkpoint write and a model read, to set the default chunk and the bounded-memory test.
3. Does the download cache resume a `.part` file (needs `read_range` of the partial and an append, which `blob.append` gives on `fs`), or restart?
4. Which backend does mlmf run on (`fs` only, or S3 for checkpoints)?

## 7. Patent note

A new capability family, so `docs/PATENT-SWEEP.md` asks for a re-sweep before it ships. Ideas to sweep (queries not yet run): ranged read of a stored value through a uniform storage trait; staged streamed write invisible until a finish call, with abort-on-drop and staging cleanup at next open; a streamed-write contract tested with a killed writer and a known-bad control. Standing conditions in `ROADMAP.md` are untouched.

## 8. Decisions asked of the PM

1. **mmap:** offer `blob.local_path(key) -> Option<PathBuf>` on `fs` only (lets mlmf keep its mapping through the layer, and is a leak of the backend's layout by design), or decline and tell mlmf to keep its native mapped read for model files and use this API for checkpoints and downloads (recommend decline until mlmf says ranged reads are not enough).
2. **Two write methods or one:** keep `write_stream` and `write_stream_atomic` (recommended) or only the atomic one (smaller, but then `s3` never qualifies).
3. **Version:** 0.13.0 (second-number change because matrix rows are added), PM to allocate.
