// SPDX-License-Identifier: MIT OR Apache-2.0
//! Contract tests for the blob leaves whose written contract sequential calls can decide.
//!
//! [`certify`] runs each leaf's test on the target, on a correct reference backend (the positive
//! control: the test can pass) and on reference backends with a [`Defect`] (the known-bad control:
//! the test can fail). A leaf is recorded **on** only when the target passed and both controls
//! behaved. Anything else is an off row with the reason.

use crate::memory::yield_now;
use crate::{BlobBackend, BlobError, Defect, MemoryBlob};
use persistant_core::{CapabilityId, Config, Controls, Reason, Row, Verdict};
use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::task::Poll;

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
}

impl Leaf {
    /// Every leaf with a test here.
    pub const ALL: [Leaf; 9] = [
        Leaf::Read,
        Leaf::Write,
        Leaf::Delete,
        Leaf::Stat,
        Leaf::List,
        Leaf::CreateExclusive,
        Leaf::ReplaceConcurrentRead,
        Leaf::Append,
        Leaf::AppendConcurrentWhole,
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
        }
    }

    /// The trial parameters of a sampled-concurrency leaf, recorded in its row (a race that did
    /// not occur in N trials is "no failure seen", and N goes in the row).
    fn trials(self) -> &'static str {
        match self {
            Leaf::CreateExclusive => "contenders=16;rounds=20;value_bytes=4096",
            Leaf::ReplaceConcurrentRead => "readers=4;replaces=40;value_bytes=65536",
            Leaf::AppendConcurrentWhole => "contenders=16;records=10;value_bytes=64",
            _ => "",
        }
    }

    fn defects(self) -> &'static [Defect] {
        match self {
            Leaf::Read => &[Defect::ReadTruncates],
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
    };
    // Best-effort cleanup so a shared target is left as found.
    if let Ok(page) = b.list(&base, None, usize::MAX).await {
        for e in page.entries {
            let _ = b.delete(&e.key).await;
        }
    }
    result
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

type Task<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;

/// Poll every future on this task until all are done. Runtime-agnostic: no spawn, so the
/// interleaving is at await points; real parallelism comes from the backend's own threads.
async fn join_all<T>(tasks: Vec<Task<'_, T>>) -> Vec<T> {
    let mut slots: Vec<Option<T>> = tasks.iter().map(|_| None).collect();
    let mut tasks: Vec<Option<Task<'_, T>>> = tasks.into_iter().map(Some).collect();
    std::future::poll_fn(|cx| {
        let mut pending = false;
        for (i, t) in tasks.iter_mut().enumerate() {
            let ready = match t.as_mut() {
                Some(fut) => match fut.as_mut().poll(cx) {
                    Poll::Ready(v) => Some(v),
                    Poll::Pending => {
                        pending = true;
                        None
                    }
                },
                None => None,
            };
            if let Some(v) = ready {
                slots[i] = Some(v);
                *t = None;
            }
        }
        if pending {
            Poll::Pending
        } else {
            Poll::Ready(())
        }
    })
    .await;
    slots.into_iter().flatten().collect()
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
    let mut out = Vec::new();
    for leaf in Leaf::ALL {
        let positive_passed = run_leaf(leaf, &MemoryBlob::new()).await.is_ok();
        let mut known_bad_failed = true;
        for d in leaf.defects() {
            if run_leaf(leaf, &MemoryBlob::with_defect(*d)).await.is_ok() {
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
        let rows = certify(&MemoryBlob::new(), &Config::new(), &Config::new()).await;
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
                let rows =
                    certify(&MemoryBlob::with_defect(*d), &Config::new(), &Config::new()).await;
                let mine = rows.iter().find(|c| c.leaf == leaf).unwrap();
                assert!(!mine.row.enabled, "{d:?} must turn {} off", leaf.id());
                assert_eq!(mine.row.reason, Some(Reason::Wrong), "{d:?}");
            }
        }
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
