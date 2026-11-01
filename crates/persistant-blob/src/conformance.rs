// SPDX-License-Identifier: MIT OR Apache-2.0
//! Contract tests for the blob leaves whose written contract sequential calls can decide.
//!
//! [`certify`] runs each leaf's test on the target, on a correct reference backend (the positive
//! control: the test can pass) and on reference backends with a [`Defect`] (the known-bad control:
//! the test can fail). A leaf is recorded **on** only when the target passed and both controls
//! behaved. Anything else is an off row with the reason.

use crate::concurrency::{Task, join_all, yield_now};
use crate::{BlobBackend, BlobError, Defect, MemoryBlob};
use persistant_core::{CapabilityId, Config, Controls, Reason, Row, Verdict};
use std::collections::BTreeMap;

/// A leaf this module can decide.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Leaf {
    /// `blob.read`
    Read,
    /// `blob.write`
    Write,
    /// `blob.delete`
    Delete,
    /// `blob.stat`
    Stat,
    /// `blob.list`
    List,
    /// `blob.create_exclusive`, by sampled concurrency
    CreateExclusive,
    /// `blob.replace_atomic.concurrent_read`, by sampled concurrency
    ReplaceConcurrentRead,
    /// `blob.append`
    Append,
    /// `blob.append.concurrent_whole`, by sampled concurrency
    AppendConcurrentWhole,
    /// `blob.read_range`
    ReadRange,
    /// `blob.read_range.consistent`
    ReadRangeConsistent,
    /// `blob.write_stream`
    WriteStream,
    /// `blob.write_stream.atomic`, by sampled concurrency
    WriteStreamAtomic,
    /// `blob.max_value_size`: on only where the backend advertises a ceiling and holds to it
    MaxValueSize,
}

impl Leaf {
    /// Every leaf with a test here.
    pub const ALL: [Leaf; 14] = [
        Leaf::Read,
        Leaf::Write,
        Leaf::Delete,
        Leaf::Stat,
        Leaf::List,
        Leaf::CreateExclusive,
        Leaf::ReplaceConcurrentRead,
        Leaf::Append,
        Leaf::AppendConcurrentWhole,
        Leaf::ReadRange,
        Leaf::ReadRangeConsistent,
        Leaf::WriteStream,
        Leaf::WriteStreamAtomic,
        Leaf::MaxValueSize,
    ];

    /// The capability id.
    pub fn id(self) -> &'static str {
        match self {
            Leaf::Read => "blob.read",
            Leaf::Write => "blob.write",
            Leaf::Delete => "blob.delete",
            Leaf::Stat => "blob.stat",
            Leaf::List => "blob.list",
            Leaf::CreateExclusive => "blob.create_exclusive",
            Leaf::ReplaceConcurrentRead => "blob.replace_atomic.concurrent_read",
            Leaf::Append => "blob.append",
            Leaf::AppendConcurrentWhole => "blob.append.concurrent_whole",
            Leaf::ReadRange => "blob.read_range",
            Leaf::ReadRangeConsistent => "blob.read_range.consistent",
            Leaf::WriteStream => "blob.write_stream",
            Leaf::WriteStreamAtomic => "blob.write_stream.atomic",
            Leaf::MaxValueSize => "blob.max_value_size",
        }
    }

    /// The trial parameters of a sampled-concurrency leaf, recorded in its row (a race that did
    /// not occur in N trials is "no failure seen", and N goes in the row).
    fn trials(self) -> &'static str {
        match self {
            Leaf::CreateExclusive => "contenders=16;rounds=20;value_bytes=4096",
            Leaf::ReplaceConcurrentRead => "readers=4;replaces=40;value_bytes=65536",
            Leaf::AppendConcurrentWhole => "contenders=16;records=10;value_bytes=64",
            Leaf::WriteStreamAtomic => "readers=4;replaces=40;value_bytes=65536",
            _ => "",
        }
    }

    fn defects(self) -> &'static [Defect] {
        match self {
            Leaf::Read => &[Defect::ReadTruncates, Defect::KeysFoldCase],
            Leaf::Write => &[Defect::WriteIsLost],
            Leaf::Delete => &[Defect::DeleteIsIgnored],
            Leaf::Stat => &[Defect::StatLiesAboutLength],
            Leaf::List => &[
                Defect::ListIsDescending,
                Defect::ListPrefixIsPattern,
                Defect::ListIgnoresPaging,
            ],
            Leaf::CreateExclusive => &[Defect::CreateNotExclusive],
            Leaf::ReplaceConcurrentRead => &[Defect::ReplaceTearsReads],
            Leaf::Append => &[Defect::AppendOverwrites],
            Leaf::AppendConcurrentWhole => &[Defect::AppendTearsRecords, Defect::AppendOverwrites],
            Leaf::ReadRange => &[Defect::RangeIgnoresOffset, Defect::RangeShort],
            Leaf::ReadRangeConsistent => &[Defect::RangeIfIgnoresTag, Defect::TagIgnoresContent],
            Leaf::MaxValueSize => &[Defect::CeilingIgnored, Defect::RefusalKeepsPrefix],
            Leaf::WriteStream => &[
                Defect::StreamWritesThrough,
                Defect::StreamDropsTail,
                Defect::AbortLeavesStaging,
            ],
            Leaf::WriteStreamAtomic => &[
                Defect::StreamAtomicTears,
                Defect::StreamWritesThrough,
                Defect::AbortLeavesStaging,
            ],
        }
    }
}

/// Why a leaf's test did not pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// The backend returned something other than the contract says.
    Wrong(String),
    /// The backend could not perform an operation the test needs.
    Backend(BlobError),
}

impl From<BlobError> for Failure {
    fn from(e: BlobError) -> Self {
        Failure::Backend(e)
    }
}

fn wrong<T>(msg: impl Into<String>) -> Result<T, Failure> {
    Err(Failure::Wrong(msg.into()))
}

/// Run one leaf's test against `b`. Writes only under `persistant-conformance/<leaf>/`.
pub async fn run_leaf<B: BlobBackend>(leaf: Leaf, b: &B) -> Result<(), Failure> {
    let base = format!("persistant-conformance/{}/", leaf.id());
    let result = match leaf {
        Leaf::Read => read(b, &base).await,
        Leaf::Write => write(b, &base).await,
        Leaf::Delete => delete(b, &base).await,
        Leaf::Stat => stat(b, &base).await,
        Leaf::List => list(b, &base).await,
        Leaf::CreateExclusive => create_exclusive(b, &base).await,
        Leaf::ReplaceConcurrentRead => replace_concurrent_read(b, &base).await,
        Leaf::Append => append(b, &base).await,
        Leaf::AppendConcurrentWhole => append_concurrent_whole(b, &base).await,
        Leaf::ReadRange => read_range(b, &base).await,
        Leaf::ReadRangeConsistent => read_range_consistent(b, &base).await,
        Leaf::WriteStream => write_stream(b, &base, false).await,
        Leaf::WriteStreamAtomic => write_stream_atomic(b, &base).await,
        Leaf::MaxValueSize => max_value_size(b, &base).await,
    };
    // Best-effort cleanup so a shared target is left as found.
    if let Ok(page) = b.list(&base, None, usize::MAX).await {
        for e in page.entries {
            let _ = b.delete(&e.key).await;
        }
    }
    result
}

/// The ceiling of the capped stores that control `blob.max_value_size`.
const CONTROL_CEILING: u64 = 1024 * 1024;

/// Ceilings larger than this are not tested (the test writes a value of the ceiling's size).
const MAX_TESTED_CEILING: u64 = 8 * 1024 * 1024;

/// `blob.max_value_size`: a backend that advertises a ceiling `n` takes a value of exactly `n`
/// bytes whole, and refuses `n + 1` by every route that writes (write, and create, replace and
/// append where the backend has them) leaving the earlier value untouched, or no value where there
/// was none. A backend that advertises no ceiling has nothing to test: the leaf is off as missing.
async fn max_value_size<B: BlobBackend>(b: &B, base: &str) -> Result<(), Failure> {
    let Some(n) = b.max_value_size() else {
        return Err(BlobError::Unsupported("no value ceiling is advertised".to_string()).into());
    };
    if n > MAX_TESTED_CEILING {
        return Err(BlobError::Unsupported(format!(
            "the {n}-byte ceiling is too large to test here"
        ))
        .into());
    }
    let len = n as usize;
    let at = pattern(len, 3);
    let over = pattern(len + 1, 5);
    let key = format!("{base}at");
    b.write(&key, at.clone()).await?;
    if b.read(&key).await?.as_deref() != Some(at.as_slice()) {
        return wrong(format!("a value of exactly {n} bytes was not stored whole"));
    }
    // Over the ceiling, on a key that holds a value: refused, and the value is as it was.
    let routes: [(&str, Result<(), BlobError>); 2] = [
        ("write", b.write(&key, over.clone()).await),
        ("append", b.append(&key, vec![7]).await),
    ];
    for (name, r) in routes {
        // An error is the refusal; a route the backend does not have is not a failure of the ceiling.
        if r.is_ok() {
            return wrong(format!("{name} over the {n}-byte ceiling was accepted"));
        }
        if b.read(&key).await?.as_deref() != Some(at.as_slice()) {
            return wrong(format!("a refused {name} changed the stored value"));
        }
    }
    // Over the ceiling, on a fresh key: refused, and no key appears.
    let fresh = format!("{base}fresh");
    if b.write(&fresh, over.clone()).await.is_ok() {
        return wrong(format!(
            "a fresh write over the {n}-byte ceiling was accepted"
        ));
    }
    if b.stat(&fresh).await?.is_some() {
        return wrong("a refused fresh write left a value behind");
    }
    let made = format!("{base}made");
    match b.create_exclusive(&made, over.clone()).await {
        Ok(_) => return wrong("create_exclusive over the ceiling was accepted"),
        Err(_) => {
            if b.stat(&made).await?.is_some() {
                return wrong("a refused create_exclusive left a value behind");
            }
        }
    }
    if let Err(e) = b.replace_atomic(&key, over).await {
        let _ = e;
        if b.read(&key).await?.as_deref() != Some(at.as_slice()) {
            return wrong("a refused replace_atomic changed the stored value");
        }
    } else {
        return wrong("replace_atomic over the ceiling was accepted");
    }
    Ok(())
}

async fn read<B: BlobBackend>(b: &B, base: &str) -> Result<(), Failure> {
    let k = format!("{base}k");
    if b.read(&k).await?.is_some() {
        return wrong("an absent key read as present");
    }
    for value in [vec![], vec![0u8], vec![1, 2, 3, 0xFF, 0x00, 0x80]] {
        b.write(&k, value.clone()).await?;
        match b.read(&k).await? {
            Some(got) if got == value => {}
            Some(got) => return wrong(format!("wrote {value:?}, read {got:?}")),
            None => return wrong(format!("wrote {value:?}, read nothing")),
        }
    }
    // Keys that differ only in letter case are different keys. A case-insensitive filesystem
    // (the Windows and default macOS ones) fails this.
    let upper = format!("{base}CaseKey");
    let lower = format!("{base}casekey");
    b.write(&upper, b"U".to_vec()).await?;
    b.write(&lower, b"l".to_vec()).await?;
    match b.read(&upper).await? {
        Some(v) if v == b"U" => {}
        other => {
            return wrong(format!(
                "keys differing only in case are the same key (read {other:?} after writing both)"
            ));
        }
    }
    Ok(())
}

async fn write<B: BlobBackend>(b: &B, base: &str) -> Result<(), Failure> {
    let k = format!("{base}nested/dir/k");
    b.write(&k, b"first".to_vec()).await?;
    b.write(&k, b"second, longer".to_vec()).await?;
    match b.read(&k).await? {
        Some(v) if v == b"second, longer" => {}
        other => return wrong(format!("after two writes read {other:?}")),
    }
    b.write(&k, b"x".to_vec()).await?;
    match b.read(&k).await? {
        Some(v) if v == b"x" => Ok(()),
        other => wrong(format!("a shorter replacement left {other:?}")),
    }
}

async fn delete<B: BlobBackend>(b: &B, base: &str) -> Result<(), Failure> {
    let k = format!("{base}k");
    b.write(&k, b"v".to_vec()).await?;
    b.delete(&k).await?;
    if b.read(&k).await?.is_some() {
        return wrong("a deleted key still reads");
    }
    b.delete(&k).await?; // deleting an absent key is not an error
    Ok(())
}

async fn stat<B: BlobBackend>(b: &B, base: &str) -> Result<(), Failure> {
    let k = format!("{base}k");
    if b.stat(&k).await?.is_some() {
        return wrong("an absent key has a stat");
    }
    for len in [0usize, 1, 5, 4096] {
        b.write(&k, vec![7u8; len]).await?;
        match b.stat(&k).await? {
            Some(e) if e.key == k && e.len == len as u64 => {}
            other => return wrong(format!("a {len}-byte value stat as {other:?}")),
        }
    }
    Ok(())
}

async fn list<B: BlobBackend>(b: &B, base: &str) -> Result<(), Failure> {
    // "1" < "10" < "2" bytewise: a numeric or natural sort would put "2" before "10".
    let under = ["a/1", "a/10", "a/2", "a/b/c"];
    for k in under
        .iter()
        .chain(["ab", "abz", "a0x", "a%z", "a[0]x", "b/1"].iter())
    {
        b.write(&format!("{base}{k}"), k.as_bytes().to_vec())
            .await?;
    }
    let full = |s: &str| format!("{base}{s}");
    let want: Vec<String> = ["a/1", "a/10", "a/2", "a/b/c"]
        .iter()
        .map(|s| full(s))
        .collect();

    // One page: literal prefix, ascending bytewise, complete.
    let all = b.list(&full("a/"), None, 100).await?;
    let got: Vec<String> = all.entries.iter().map(|e| e.key.clone()).collect();
    if got != want || all.next.is_some() {
        return wrong(format!(
            "prefix a/ listed {got:?} next {:?}, want {want:?}",
            all.next
        ));
    }
    if all.entries.iter().any(|e| e.len == 0) {
        return wrong("a listed entry reports length 0 for a non-empty value");
    }

    // The prefix is literal: `%` (SQL LIKE) and `[` (glob class) match only themselves. These
    // are valid in a Windows file name; `*` and `?` are not, so they cannot be used here.
    for (prefix, only) in [("a%", "a%z"), ("a[0]", "a[0]x")] {
        let p = b.list(&full(prefix), None, 100).await?;
        let got: Vec<String> = p.entries.iter().map(|e| e.key.clone()).collect();
        if got != vec![full(only)] {
            return wrong(format!(
                "prefix {prefix} listed {got:?}, want only {:?}",
                full(only)
            ));
        }
    }

    // Paging: limit 2 over 4 keys, each key once, continuation ends with None.
    let mut seen = Vec::new();
    let mut after: Option<String> = None;
    for _ in 0..10 {
        let p = b.list(&full("a/"), after.as_deref(), 2).await?;
        if p.entries.len() > 2 {
            return wrong(format!(
                "a page of limit 2 held {} entries",
                p.entries.len()
            ));
        }
        seen.extend(p.entries.iter().map(|e| e.key.clone()));
        match p.next {
            Some(c) => after = Some(c),
            None => break,
        }
    }
    if seen != want {
        return wrong(format!("paged listing returned {seen:?}, want {want:?}"));
    }
    Ok(())
}

async fn create_exclusive<B: BlobBackend>(b: &B, base: &str) -> Result<(), Failure> {
    const CONTENDERS: usize = 16;
    const ROUNDS: usize = 20;
    let payload = |i: usize| vec![i as u8; 4096];
    for round in 0..ROUNDS {
        let key = format!("{base}r{round}");
        let key = key.as_str();
        let tasks: Vec<Task<'_, Result<bool, BlobError>>> = (0..CONTENDERS)
            .map(|i| -> Task<'_, _> { Box::pin(b.create_exclusive(key, payload(i))) })
            .collect();
        let results = join_all(tasks).await;
        let mut winners = Vec::new();
        for (i, r) in results.into_iter().enumerate() {
            if r? {
                winners.push(i);
            }
        }
        if winners.len() != 1 {
            return wrong(format!(
                "round {round}: {} of {CONTENDERS} concurrent creators succeeded, want exactly 1",
                winners.len()
            ));
        }
        match b.read(key).await? {
            Some(v) if v == payload(winners[0]) => {}
            other => {
                return wrong(format!(
                    "round {round}: the stored value is not the winner's ({} bytes)",
                    other.map_or(0, |v| v.len())
                ));
            }
        }
    }
    Ok(())
}

async fn replace_concurrent_read<B: BlobBackend>(b: &B, base: &str) -> Result<(), Failure> {
    const LEN: usize = 65536;
    const READERS: usize = 4;
    const REPLACES: usize = 40;
    let key = format!("{base}k");
    let key = key.as_str();
    let a = vec![b'A'; LEN];
    let c = vec![b'B'; LEN];
    b.replace_atomic(key, a.clone()).await?;
    let done = std::cell::Cell::new(false);
    let done = &done;
    let mut tasks: Vec<Task<'_, Result<(), Failure>>> = Vec::new();
    tasks.push(Box::pin(async move {
        for i in 0..REPLACES {
            let v = if i % 2 == 0 { c.clone() } else { a.clone() };
            if let Err(e) = b.replace_atomic(key, v).await {
                done.set(true);
                return Err(Failure::from(e));
            }
            yield_now().await;
        }
        done.set(true);
        Ok(())
    }));
    for _ in 0..READERS {
        tasks.push(Box::pin(async move {
            for _ in 0..(REPLACES * 20) {
                match b.read(key).await {
                    Ok(Some(v)) => {
                        let uniform = v.len() == LEN
                            && v.iter().all(|x| *x == v[0])
                            && (v[0] == b'A' || v[0] == b'B');
                        if !uniform {
                            done.set(true);
                            return wrong(format!(
                                "a reader saw a mix of old and new ({} bytes, first {:?})",
                                v.len(),
                                v.first()
                            ));
                        }
                    }
                    Ok(None) => {
                        done.set(true);
                        return wrong("the key vanished during a replace");
                    }
                    Err(e) => {
                        done.set(true);
                        return Err(Failure::from(e));
                    }
                }
                if done.get() {
                    break;
                }
                yield_now().await;
            }
            Ok(())
        }));
    }
    for r in join_all(tasks).await {
        r?;
    }
    Ok(())
}

async fn append<B: BlobBackend>(b: &B, base: &str) -> Result<(), Failure> {
    let k = format!("{base}k");
    b.write(&k, b"head".to_vec()).await?;
    for piece in [&b"a"[..], b"bc", b"", b"def"] {
        b.append(&k, piece.to_vec()).await?;
    }
    match b.read(&k).await? {
        Some(v) if v == b"headabcdef" => Ok(()),
        other => wrong(format!(
            "appends in order left {other:?}, want \"headabcdef\""
        )),
    }
}

async fn append_concurrent_whole<B: BlobBackend>(b: &B, base: &str) -> Result<(), Failure> {
    const CONTENDERS: usize = 16;
    const RECORDS: usize = 10;
    const LEN: usize = 64;
    let key = format!("{base}k");
    let key = key.as_str();
    b.write(key, Vec::new()).await?;
    let tasks: Vec<Task<'_, Result<(), BlobError>>> = (0..CONTENDERS)
        .map(|i| -> Task<'_, _> {
            Box::pin(async move {
                for _ in 0..RECORDS {
                    b.append(key, vec![i as u8; LEN]).await?;
                    yield_now().await;
                }
                Ok(())
            })
        })
        .collect();
    for r in join_all(tasks).await {
        r?;
    }
    let all = b.read(key).await?.unwrap_or_default();
    if all.len() != CONTENDERS * RECORDS * LEN {
        return wrong(format!(
            "{} bytes after {} appends of {LEN}, want {}",
            all.len(),
            CONTENDERS * RECORDS,
            CONTENDERS * RECORDS * LEN
        ));
    }
    let mut counts = [0usize; CONTENDERS];
    for chunk in all.chunks(LEN) {
        let first = chunk[0] as usize;
        if first >= CONTENDERS || chunk.iter().any(|x| *x as usize != first) {
            return wrong("a record is interleaved with another's bytes");
        }
        counts[first] += 1;
    }
    if counts.iter().any(|c| *c != RECORDS) {
        return wrong(format!(
            "records per appender {counts:?}, want {RECORDS} each"
        ));
    }
    Ok(())
}

/// A position-dependent pattern, so a range read from the wrong offset differs from the right one.
fn pattern(len: usize, seed: u8) -> Vec<u8> {
    (0..len)
        .map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed))
        .collect()
}

async fn read_range<B: BlobBackend>(b: &B, base: &str) -> Result<(), Failure> {
    const LEN: usize = 300_000;
    let k = format!("{base}k");
    let v = pattern(LEN, 7);
    b.write(&k, v.clone()).await?;
    let l = LEN as u64;
    let cases: [(u64, u64); 9] = [
        (0, 10),
        (1, 10),
        (l / 2, 100),
        (l - 1, 10),
        (l + 5, 10),
        (l, 5),
        (5, 0),
        (0, l),
        (0, l * 2),
    ];
    for (offset, len) in cases {
        let start = (offset as usize).min(LEN);
        let end = (start + len as usize).min(LEN);
        match b.read_range(&k, offset, len).await? {
            Some(got) if got == v[start..end] => {}
            other => {
                return wrong(format!(
                    "range ({offset}, {len}) gave {} bytes, want the {} bytes of the whole value at that place: {}",
                    other.as_ref().map_or(0, Vec::len),
                    end - start,
                    if other.is_some() {
                        "different bytes"
                    } else {
                        "None"
                    },
                ));
            }
        }
    }
    if b.read_range(&format!("{base}absent"), 0, 10)
        .await?
        .is_some()
    {
        return wrong("a range of an absent key was not None");
    }
    Ok(())
}

async fn read_range_consistent<B: BlobBackend>(b: &B, base: &str) -> Result<(), Failure> {
    let k = format!("{base}k");
    let a = pattern(1000, 1);
    b.write(&k, a.clone()).await?;
    let Some(tag) = b.stat(&k).await?.and_then(|e| e.tag) else {
        return Err(Failure::Backend(BlobError::Unsupported(
            "stat returns no version tag".to_string(),
        )));
    };
    match b.read_range_if(&k, 10, 20, &tag).await? {
        Some(got) if got == a[10..30] => {}
        other => return wrong(format!("a range under the current tag gave {other:?}")),
    }
    // Same length, other bytes: a length comparison cannot see it, the tag must.
    let c = pattern(1000, 2);
    b.write(&k, c.clone()).await?;
    match b.read_range_if(&k, 10, 20, &tag).await {
        Err(BlobError::Changed(_)) => {}
        Ok(r) => return wrong(format!("a same-length replacement went unnoticed: {r:?}")),
        Err(e) => return Err(Failure::from(e)),
    }
    let Some(fresh) = b.stat(&k).await?.and_then(|e| e.tag) else {
        return wrong("stat lost its tag");
    };
    if fresh == tag {
        return wrong("the tag did not change with the value");
    }
    match b.read_range_if(&k, 10, 20, &fresh).await? {
        Some(got) if got == c[10..30] => {}
        other => return wrong(format!("a range under the new tag gave {other:?}")),
    }
    if b.read_range_if(&format!("{base}absent"), 0, 1, &fresh)
        .await?
        .is_some()
    {
        return wrong("a conditional range of an absent key was not None");
    }
    Ok(())
}

async fn open_sink<B: BlobBackend>(
    b: &B,
    key: &str,
    atomic: bool,
) -> Result<Box<dyn crate::BlobSink>, BlobError> {
    if atomic {
        b.write_stream_atomic(key).await
    } else {
        b.write_stream(key).await
    }
}

/// The keys under `base` that a `list` shows: a streamed write leaves no staging among them.
async fn keys_under<B: BlobBackend>(b: &B, base: &str) -> Result<Vec<String>, Failure> {
    let page = b.list(base, None, usize::MAX).await?;
    Ok(page.entries.into_iter().map(|e| e.key).collect())
}

async fn write_stream<B: BlobBackend>(b: &B, base: &str, atomic: bool) -> Result<(), Failure> {
    let k = format!("{base}k");
    // Chunked writes of 1, 1000 and 65536 bytes make the same value as one whole write.
    for chunk in [1usize, 1000, 65_536] {
        let want = pattern(200_000, chunk as u8);
        let mut sink = open_sink(b, &k, atomic).await?;
        if chunk == 1 {
            // Only a prefix is streamed byte by byte, to keep the test fast; the rest in one piece.
            for byte in &want[..64] {
                sink.write(std::slice::from_ref(byte)).await?;
            }
            sink.write(&want[64..]).await?;
        } else {
            for piece in want.chunks(chunk) {
                sink.write(piece).await?;
            }
        }
        if chunk == 1 && b.read(&k).await?.is_some() {
            return wrong("a value was visible at a new key before finish");
        }
        sink.finish().await?;
        match b.read(&k).await? {
            Some(got) if got == want => {}
            other => {
                return wrong(format!(
                    "chunks of {chunk} bytes left {:?} bytes, want the {} written",
                    other.as_ref().map(Vec::len),
                    want.len()
                ));
            }
        }
    }
    // An existing key keeps its complete old bytes until finish (a same-length overwrite in place
    // must be seen, so the bytes are compared, not the length).
    let old = pattern(50_000, 9);
    let new = pattern(50_000, 10);
    b.write(&k, old.clone()).await?;
    let mut sink = open_sink(b, &k, atomic).await?;
    sink.write(&new[..25_000]).await?;
    if b.read(&k).await?.as_deref() != Some(&old[..]) {
        return wrong("the old value changed before finish");
    }
    sink.write(&new[25_000..]).await?;
    if b.read(&k).await?.as_deref() != Some(&old[..]) {
        return wrong("the old value changed before finish");
    }
    sink.finish().await?;
    if b.read(&k).await?.as_deref() != Some(&new[..]) {
        return wrong("finish did not publish the new value");
    }
    // Abort and drop leave the old value and no staging a list can see; the next stream works.
    for how in ["abort", "drop"] {
        b.write(&k, old.clone()).await?;
        let mut sink = open_sink(b, &k, atomic).await?;
        sink.write(&new[..10_000]).await?;
        if how == "abort" {
            sink.abort().await?;
        } else {
            drop(sink);
        }
        if b.read(&k).await?.as_deref() != Some(&old[..]) {
            return wrong(format!("{how} changed the old value"));
        }
        let mut next = open_sink(b, &k, atomic).await?;
        next.write(&new[..100]).await?;
        let keys = keys_under(b, base).await?;
        if keys != [k.clone()] {
            return wrong(format!(
                "after {how} the store lists {keys:?}, want only the key"
            ));
        }
        next.abort().await?;
    }
    Ok(())
}

async fn write_stream_atomic<B: BlobBackend>(b: &B, base: &str) -> Result<(), Failure> {
    write_stream(b, base, true).await?;
    const LEN: usize = 65536;
    const READERS: usize = 4;
    const REPLACES: usize = 40;
    let key = format!("{base}c");
    let key = key.as_str();
    let a = vec![b'A'; LEN];
    let c = vec![b'B'; LEN];
    b.write(key, a.clone()).await?;
    let done = std::cell::Cell::new(false);
    let done = &done;
    let mut tasks: Vec<Task<'_, Result<(), Failure>>> = Vec::new();
    tasks.push(Box::pin(async move {
        for i in 0..REPLACES {
            let v = if i % 2 == 0 { &c } else { &a };
            let r = async {
                let mut sink = b.write_stream_atomic(key).await?;
                sink.write(&v[..LEN / 2]).await?;
                sink.write(&v[LEN / 2..]).await?;
                sink.finish().await
            }
            .await;
            if let Err(e) = r {
                done.set(true);
                return Err(Failure::from(e));
            }
            yield_now().await;
        }
        done.set(true);
        Ok(())
    }));
    for _ in 0..READERS {
        tasks.push(Box::pin(async move {
            for _ in 0..(REPLACES * 20) {
                match b.read(key).await {
                    Ok(Some(v)) => {
                        let uniform = v.len() == LEN
                            && v.iter().all(|x| *x == v[0])
                            && (v[0] == b'A' || v[0] == b'B');
                        if !uniform {
                            done.set(true);
                            return wrong(format!(
                                "a reader saw a mix of old and new ({} bytes, first {:?})",
                                v.len(),
                                v.first()
                            ));
                        }
                    }
                    Ok(None) => {
                        done.set(true);
                        return wrong("the key vanished during a streamed replace");
                    }
                    Err(e) => {
                        done.set(true);
                        return Err(Failure::from(e));
                    }
                }
                if done.get() {
                    break;
                }
                yield_now().await;
            }
            Ok(())
        }));
    }
    for r in join_all(tasks).await {
        r?;
    }
    Ok(())
}

/// The outcome of certifying one leaf.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Certified {
    /// The leaf.
    pub leaf: Leaf,
    /// The row to record.
    pub row: Row,
}

/// Run every leaf here against `target` and return one row per leaf.
///
/// `config` is the facts the gate can read for this target (`collation=...`); `metadata` is
/// copied into every row, with `test=blob::<leaf>` added. A leaf is **on** only if the target
/// passed *and* the positive control passed *and* every known-bad control failed.
pub async fn certify<B: BlobBackend>(
    target: &B,
    config: &Config,
    metadata: &Config,
) -> Vec<Certified> {
    certify_leaves(target, config, metadata, &Leaf::ALL).await
}

/// As [`certify`], for the listed leaves only (each with its controls).
pub async fn certify_leaves<B: BlobBackend>(
    target: &B,
    config: &Config,
    metadata: &Config,
    leaves: &[Leaf],
) -> Vec<Certified> {
    let mut out = Vec::new();
    for leaf in leaves.iter().copied() {
        // The ceiling leaf's controls are capped stores: an uncapped one has nothing to test.
        let capped = |m: MemoryBlob| {
            if leaf == Leaf::MaxValueSize {
                m.with_ceiling(CONTROL_CEILING)
            } else {
                m
            }
        };
        let positive_passed = run_leaf(leaf, &capped(MemoryBlob::new())).await.is_ok();
        let mut known_bad_failed = true;
        for d in leaf.defects() {
            if run_leaf(leaf, &capped(MemoryBlob::with_defect(*d)))
                .await
                .is_ok()
            {
                known_bad_failed = false;
            }
        }
        let controls = Controls {
            positive_passed,
            known_bad_failed,
        };
        let result = run_leaf(leaf, target).await;
        let mut md = metadata
            .clone()
            .with("test", format!("blob::{}", leaf.id()));
        if let Ok(t) = persistant_core::Config::parse(leaf.trials()) {
            for key in [
                "contenders",
                "rounds",
                "value_bytes",
                "readers",
                "replaces",
                "records",
            ] {
                if let Some(v) = t.get(key) {
                    md = md.with(key, v);
                }
            }
        }
        let id = CapabilityId::new(leaf.id()).expect("leaf id is valid");
        let row = match (
            &result,
            controls.positive_passed && controls.known_bad_failed,
        ) {
            (Ok(()), true) => Verdict::Exact
                .into_row(
                    id,
                    target.backend(),
                    target.version(),
                    config.clone(),
                    md,
                    controls,
                )
                .expect("both controls hold"),
            (Ok(()), false) => off(
                id,
                target,
                config,
                md,
                Reason::Untested,
                format!("controls did not hold: {controls:?}"),
            ),
            (Err(Failure::Wrong(m)), _) => off(id, target, config, md, Reason::Wrong, m.clone()),
            (Err(Failure::Backend(e)), _) => {
                off(id, target, config, md, Reason::Missing, e.to_string())
            }
        };
        out.push(Certified { leaf, row });
    }
    out
}

fn off<B: BlobBackend>(
    subject: CapabilityId,
    target: &B,
    config: &Config,
    metadata: Config,
    reason: Reason,
    note: String,
) -> Row {
    Row {
        subject_kind: "capability".to_string(),
        subject,
        backend: target.backend().to_string(),
        version: target.version().to_string(),
        config: config.clone(),
        enabled: false,
        reason: Some(reason),
        metadata,
        note: note.replace(['\t', '\n'], " "),
        extra: BTreeMap::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_correct_backend_is_on_for_every_leaf() {
        let rows = certify(
            &MemoryBlob::new().with_ceiling(CONTROL_CEILING),
            &Config::new(),
            &Config::new(),
        )
        .await;
        assert_eq!(rows.len(), Leaf::ALL.len());
        for c in &rows {
            assert!(c.row.enabled, "{}: {}", c.leaf.id(), c.row.note);
            assert!(c.row.metadata.get("test").is_some());
        }
    }

    #[tokio::test]
    async fn every_defect_turns_its_own_leaf_off_as_wrong() {
        for leaf in Leaf::ALL {
            for d in leaf.defects() {
                let target = MemoryBlob::with_defect(*d).with_ceiling(CONTROL_CEILING);
                let rows = certify(&target, &Config::new(), &Config::new()).await;
                let mine = rows.iter().find(|c| c.leaf == leaf).unwrap();
                assert!(!mine.row.enabled, "{d:?} must turn {} off", leaf.id());
                assert_eq!(mine.row.reason, Some(Reason::Wrong), "{d:?}");
            }
        }
    }

    #[tokio::test]
    async fn a_backend_with_no_ceiling_is_off_as_missing_for_max_value_size() {
        let rows = certify(&MemoryBlob::new(), &Config::new(), &Config::new()).await;
        let mine = rows.iter().find(|c| c.leaf == Leaf::MaxValueSize).unwrap();
        assert!(!mine.row.enabled);
        assert_eq!(mine.row.reason, Some(Reason::Missing));
        assert!(
            mine.row.note.contains("no value ceiling"),
            "{}",
            mine.row.note
        );
        // The control: every other leaf is still on for the same store.
        assert!(rows.iter().filter(|c| c.row.enabled).count() >= Leaf::ALL.len() - 1);
    }

    #[tokio::test]
    async fn the_harness_can_tell_leaves_apart() {
        // Control: ReadTruncates breaks blob.read and leaves blob.delete on.
        let rows = certify(
            &MemoryBlob::with_defect(Defect::ReadTruncates),
            &Config::new(),
            &Config::new(),
        )
        .await;
        let by = |l: Leaf| rows.iter().find(|c| c.leaf == l).unwrap().row.enabled;
        assert!(!by(Leaf::Read));
        assert!(by(Leaf::Stat), "a read defect must not turn stat off");
    }

    #[tokio::test]
    async fn rows_round_trip_through_the_matrix_and_the_gate_reads_them() {
        let rows: Vec<Row> = certify(&MemoryBlob::new(), &Config::new(), &Config::new())
            .await
            .into_iter()
            .map(|c| c.row)
            .collect();
        let m = persistant_core::Matrix::parse(&persistant_core::Matrix::from_rows(rows).to_tsv())
            .unwrap();
        let id = CapabilityId::new("blob.list").unwrap();
        let v = env!("CARGO_PKG_VERSION");
        assert!(m.enabled(&id, "memory", v, &Config::new()));
        assert!(
            !m.enabled(&id, "memory", "0.0.0-other", &Config::new()),
            "unknown version is off"
        );
    }
}
