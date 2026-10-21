// SPDX-License-Identifier: MIT OR Apache-2.0
//! Contract tests for the key-value leaves this crate can decide.
//!
//! [`certify`] has the same shape as `persistant_blob::conformance::certify`: each leaf is run on
//! the target, on a correct reference (positive control) and on defective references (known-bad
//! control), and is recorded **on** only when all three behaved.
//!
//! Time to live is tested with real time: an entry written with a 1000 ms time to live is read back
//! inside the window and not after a 1200 ms sleep. The sleep blocks the calling thread. The window
//! is wall-clock time on whatever machine runs the test, so on a loaded runner a correct backend can
//! miss it (a 300 ms window did once, on a shared CI runner, 2026-10-16): that is a limit of the
//! method, not a verdict on the backend, which is why the leaf gets three attempts.
//!
//! **A server's clock is not the client's.** Expiry is judged by the server and the sleep by the
//! client, so a time-to-live leaf is only as steady as the server's clock. Measured 2026-10-16 on a
//! Windows developer machine: the clock inside the Docker Desktop VM that ran MySQL stepped by up to
//! about 1.2 s against the host within minutes (offset range -1209..+862 ms over six minutes, round
//! trips under 20 ms). That made `kv.ordered_scan_values` ("a 100 ms entry is gone after 250 ms") come
//! back wrong once in about thirty full certifications, with the expired key still in the page. A Linux
//! CI runner shares its kernel's clock with its service containers and has not shown it. Every leaf
//! that waits on the clock is retried (`Leaf::wall_clock`, exhaustive so a new leaf must choose), and a
//! defect fails every attempt.
//! `kv.ttl_remaining`, `kv.compare_and_swap`, `kv.take`, `kv.batch_atomic` and `kv.count` have no
//! test here, so no row is produced and they stay off.

use crate::{BatchOp, Defect, KvBackend, KvError, MemoryKv};
use persistant_blob::concurrency::{Task, join_all, yield_now};
use persistant_core::{CapabilityId, Config, Controls, Reason, Row, Verdict};
use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

/// A leaf this module can decide.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Leaf {
    /// `kv.get`
    Get,
    /// `kv.put`
    Put,
    /// `kv.delete`
    Delete,
    /// `kv.ttl`
    Ttl,
    /// `kv.put_if_absent`, by sampled concurrency
    PutIfAbsent,
    /// `kv.prefix_list`
    PrefixList,
    /// `kv.ordered_scan`
    OrderedScan,
    /// `kv.compare_and_swap`, by sampled concurrency
    CompareAndSwap,
    /// `kv.compare_and_delete`, by sampled concurrency
    CompareAndDelete,
    /// `lock.lease`, by real time and sampled concurrency
    Lease,
    /// `kv.ttl_remaining`, in real time
    TtlRemaining,
    /// `kv.purge_expired`, in real time
    PurgeExpired,
    /// `kv.batch_atomic.rollback`, by sampled concurrency
    BatchRollback,
    /// `kv.batch_atomic.concurrent_read`, by sampled concurrency
    BatchConcurrentRead,
    /// `kv.ordered_scan_values`
    ScanValues,
    /// `kv.batch_atomic.optimistic_sequence`: the Synapse mailbox pattern (read, then a batch of
    /// `Require` and `Put`, retried on refusal), by sampled concurrency
    OptimisticSequence,
    /// `kv.batch_atomic.absent_race`: concurrent read-then-batch committers racing for a key that is
    /// absent at the start (the Synapse mailbox's first commits); the losers get `Ok(false)`, never
    /// an error (a deadlock victim, say), by sampled concurrency
    AbsentRace,
    /// `kv.bounded_call`: a batch blocked on a lock returns [`KvError::TimedOut`] within the
    /// configured bound, leaves nothing applied, and the store works afterwards. Off (missing) for a
    /// backend with no timeout configured.
    BoundedCall,
}

impl Leaf {
    /// Every leaf with a test here.
    pub const ALL: [Leaf; 18] = [
        Leaf::Get,
        Leaf::Put,
        Leaf::Delete,
        Leaf::Ttl,
        Leaf::PutIfAbsent,
        Leaf::PrefixList,
        Leaf::OrderedScan,
        Leaf::CompareAndSwap,
        Leaf::CompareAndDelete,
        Leaf::Lease,
        Leaf::TtlRemaining,
        Leaf::PurgeExpired,
        Leaf::BatchRollback,
        Leaf::BatchConcurrentRead,
        Leaf::ScanValues,
        Leaf::OptimisticSequence,
        Leaf::AbsentRace,
        Leaf::BoundedCall,
    ];

    /// The capability id.
    pub fn id(self) -> &'static str {
        match self {
            Leaf::Get => "kv.get",
            Leaf::Put => "kv.put",
            Leaf::Delete => "kv.delete",
            Leaf::Ttl => "kv.ttl",
            Leaf::PutIfAbsent => "kv.put_if_absent",
            Leaf::PrefixList => "kv.prefix_list",
            Leaf::OrderedScan => "kv.ordered_scan",
            Leaf::CompareAndSwap => "kv.compare_and_swap",
            Leaf::CompareAndDelete => "kv.compare_and_delete",
            Leaf::Lease => "lock.lease",
            Leaf::TtlRemaining => "kv.ttl_remaining",
            Leaf::PurgeExpired => "kv.purge_expired",
            Leaf::BatchRollback => "kv.batch_atomic.rollback",
            Leaf::BatchConcurrentRead => "kv.batch_atomic.concurrent_read",
            Leaf::ScanValues => "kv.ordered_scan_values",
            Leaf::OptimisticSequence => "kv.batch_atomic.optimistic_sequence",
            Leaf::AbsentRace => "kv.batch_atomic.absent_race",
            Leaf::BoundedCall => "kv.bounded_call",
        }
    }

    fn trials(self) -> &'static str {
        match self {
            Leaf::PutIfAbsent => "contenders=16;rounds=20;value_bytes=1024",
            Leaf::Ttl => "ttl_ms=1000;sleep_ms=1200",
            Leaf::CompareAndSwap | Leaf::CompareAndDelete => "contenders=16;rounds=20",
            Leaf::Lease => "contenders=16;rounds=5;ttl_ms=200;sleep_ms=300",
            Leaf::TtlRemaining => "ttl_ms=10000;sleep_ms=150",
            Leaf::PurgeExpired => "ttl_ms=100;sleep_ms=250",
            Leaf::BatchRollback => "contenders=16;rounds=20",
            Leaf::BatchConcurrentRead => "readers=4;batches=60",
            Leaf::OptimisticSequence => "contenders=8;rounds=10",
            Leaf::AbsentRace => "contenders=8;commits=3;rounds=10",
            _ => "",
        }
    }

    /// Whether the leaf's test waits on the clock (a time to live set, then a sleep, then a check).
    /// **Exhaustive on purpose**: a new leaf must say which it is. `kv.ordered_scan_values` was
    /// missing from the retry list and failed once on a MySQL whose clock had stepped back (see the
    /// module docs); a `_` arm here is how that happens again.
    fn wall_clock(self) -> bool {
        match self {
            Leaf::Ttl
            | Leaf::TtlRemaining
            | Leaf::PurgeExpired
            | Leaf::Lease
            | Leaf::PrefixList
            | Leaf::ScanValues => true,
            Leaf::Get
            | Leaf::Put
            | Leaf::Delete
            | Leaf::PutIfAbsent
            | Leaf::OrderedScan
            | Leaf::CompareAndSwap
            | Leaf::CompareAndDelete
            | Leaf::BatchRollback
            | Leaf::BatchConcurrentRead
            | Leaf::OptimisticSequence
            | Leaf::AbsentRace
            | Leaf::BoundedCall => false,
        }
    }

    fn defects(self) -> &'static [Defect] {
        match self {
            Leaf::Get => &[Defect::PutIsLost, Defect::KeysFoldCase],
            Leaf::Put => &[Defect::PutIsLost],
            Leaf::Delete => &[Defect::DeleteIsIgnored],
            Leaf::Ttl => &[Defect::TtlIgnored, Defect::ListShowsExpired],
            Leaf::PutIfAbsent => &[Defect::PutIfAbsentNotExclusive],
            Leaf::PrefixList => &[
                Defect::ListPrefixIsPattern,
                Defect::ListIgnoresPaging,
                Defect::ListShowsExpired,
            ],
            Leaf::OrderedScan => &[
                Defect::ScanIsDescending,
                Defect::ScanIsCaseInsensitive,
                Defect::ListIgnoresPaging,
            ],
            Leaf::CompareAndSwap => &[Defect::CasIgnoresExpected, Defect::CasNotAtomic],
            Leaf::CompareAndDelete => &[Defect::CadIgnoresExpected, Defect::CadNotAtomic],
            Leaf::TtlRemaining => &[Defect::TtlRemainingIsConstant, Defect::TtlIgnored],
            Leaf::PurgeExpired => &[Defect::PurgeRemovesLive],
            Leaf::BatchRollback => &[Defect::BatchNotAtomic, Defect::BatchIgnoresRequire],
            Leaf::BatchConcurrentRead => &[Defect::BatchNotAtomic],
            Leaf::ScanValues => &[Defect::ScanIsDescending, Defect::ListIgnoresPaging],
            Leaf::OptimisticSequence => &[Defect::BatchIgnoresRequire, Defect::BatchNotAtomic],
            Leaf::AbsentRace => &[Defect::BatchIgnoresRequire],
            Leaf::BoundedCall => &[Defect::BoundIgnored, Defect::TimeoutLeavesBatchApplied],
            Leaf::Lease => &[
                Defect::PutIfAbsentNotExclusive,
                Defect::CasIgnoresExpected,
                Defect::CadIgnoresExpected,
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
    Backend(KvError),
}

impl From<KvError> for Failure {
    fn from(e: KvError) -> Self {
        Failure::Backend(e)
    }
}

fn wrong<T>(msg: impl Into<String>) -> Result<T, Failure> {
    Err(Failure::Wrong(msg.into()))
}

/// Run one leaf's test against `b`. Writes only under `persistant-conformance/<leaf>/`.
pub async fn run_leaf<K: KvBackend>(leaf: Leaf, b: &K) -> Result<(), Failure> {
    // Leaves that depend on wall-clock windows get three attempts, so a stalled machine (or a
    // server whose clock steps) cannot turn a correct backend off. A defective backend fails every
    // attempt: its failure is deterministic, not a timing accident.
    let attempts = if leaf.wall_clock() { 3 } else { 1 };
    let mut result = Ok(());
    for _ in 0..attempts {
        result = run_once(leaf, b).await;
        if result.is_ok() {
            break;
        }
    }
    result
}

/// Run one leaf's test exactly once, with no retry (see [`run_leaf`], which retries the leaves
/// that depend on wall-clock windows). For statistics: a retry would hide a stall.
pub async fn run_leaf_once<K: KvBackend>(leaf: Leaf, b: &K) -> Result<(), Failure> {
    run_once(leaf, b).await
}

async fn run_once<K: KvBackend>(leaf: Leaf, b: &K) -> Result<(), Failure> {
    let base = format!("persistant-conformance/{}/", leaf.id());
    let result = match leaf {
        Leaf::Get => get(b, &base).await,
        Leaf::Put => put(b, &base).await,
        Leaf::Delete => delete(b, &base).await,
        Leaf::Ttl => ttl(b, &base).await,
        Leaf::PutIfAbsent => put_if_absent(b, &base).await,
        Leaf::PrefixList => prefix_list(b, &base).await,
        Leaf::OrderedScan => ordered_scan(b, &base).await,
        Leaf::CompareAndSwap => compare_and_swap(b, &base).await,
        Leaf::CompareAndDelete => compare_and_delete(b, &base).await,
        Leaf::Lease => lease(b, &base).await,
        Leaf::TtlRemaining => ttl_remaining(b, &base).await,
        Leaf::PurgeExpired => purge_expired(b, &base).await,
        Leaf::BatchRollback => batch_rollback(b, &base).await,
        Leaf::BatchConcurrentRead => batch_concurrent_read(b, &base).await,
        Leaf::ScanValues => scan_values(b, &base).await,
        Leaf::OptimisticSequence => optimistic_sequence(b, &base).await,
        Leaf::AbsentRace => absent_race(b, &base).await,
        Leaf::BoundedCall => bounded_call(b).await,
    };
    // Best-effort cleanup so a shared target is left as found.
    if let Ok(page) = b.prefix_list(&base, None, usize::MAX).await {
        for k in page.keys {
            let _ = b.delete(&k).await;
        }
    }
    result
}

async fn get<K: KvBackend>(b: &K, base: &str) -> Result<(), Failure> {
    let k = format!("{base}k");
    if b.get(&k).await?.is_some() {
        return wrong("an absent key read as present");
    }
    for value in [vec![], vec![0u8], vec![1, 2, 3, 0xFF, 0x00, 0x80]] {
        b.put(&k, value.clone(), None).await?;
        match b.get(&k).await? {
            Some(got) if got == value => {}
            other => return wrong(format!("put {value:?}, get {other:?}")),
        }
    }
    // Keys that differ only in letter case are different keys.
    let upper = format!("{base}CaseKey");
    let lower = format!("{base}casekey");
    b.put(&upper, b"U".to_vec(), None).await?;
    b.put(&lower, b"l".to_vec(), None).await?;
    match b.get(&upper).await? {
        Some(v) if v == b"U" => {}
        other => {
            return wrong(format!(
                "keys differing only in case are the same key (get {other:?} after putting both)"
            ));
        }
    }
    Ok(())
}

async fn put<K: KvBackend>(b: &K, base: &str) -> Result<(), Failure> {
    let k = format!("{base}nested/k");
    b.put(&k, b"first".to_vec(), None).await?;
    b.put(&k, b"second, longer".to_vec(), None).await?;
    b.put(&k, b"x".to_vec(), None).await?;
    match b.get(&k).await? {
        Some(v) if v == b"x" => Ok(()),
        other => wrong(format!("after three puts get returned {other:?}")),
    }
}

async fn delete<K: KvBackend>(b: &K, base: &str) -> Result<(), Failure> {
    let k = format!("{base}k");
    b.put(&k, b"v".to_vec(), None).await?;
    b.delete(&k).await?;
    if b.get(&k).await?.is_some() {
        return wrong("a deleted key still reads");
    }
    b.delete(&k).await?; // deleting an absent key is not an error
    Ok(())
}

async fn ttl<K: KvBackend>(b: &K, base: &str) -> Result<(), Failure> {
    let short = format!("{base}a/short");
    let live = format!("{base}a/live");
    // "Readable inside its window" is only judged when the window was still open: the time from
    // before the put to after the get is measured on this machine's monotonic clock, and a read that
    // missed after the window had already passed (a stalled CI machine, a paused VM) says nothing
    // about the backend, so the check is repeated with a window four times longer. A backend that
    // misses while the window was open fails at once, whatever the window.
    let mut window_ms = 1000u64;
    loop {
        let started = Instant::now();
        b.put(
            &short,
            b"s".to_vec(),
            Some(Duration::from_millis(window_ms)),
        )
        .await?;
        b.put(&live, b"l".to_vec(), None).await?;
        let seen = b.get(&short).await?;
        let elapsed = started.elapsed();
        if seen.as_deref() == Some(&b"s"[..]) {
            break;
        }
        if elapsed < Duration::from_millis(window_ms) {
            return wrong("an entry with a time to live was not readable inside its window");
        }
        if window_ms >= 16_000 {
            return wrong(
                "the machine stalled past every window tried; no verdict on the time to live",
            );
        }
        eprintln!(
            "kv.ttl: the {window_ms} ms window had already passed ({elapsed:?}) when the read returned; the machine stalled, so this read says nothing about the backend. Repeating with a {} ms window.",
            window_ms * 4
        );
        window_ms *= 4;
    }
    std::thread::sleep(Duration::from_millis(window_ms + 200));
    if b.get(&short).await?.is_some() {
        return wrong("an expired entry is still returned by get");
    }
    if b.get(&live).await?.as_deref() != Some(&b"l"[..]) {
        return wrong("an entry written without a time to live disappeared");
    }
    // The contract says expired entries are invisible to listing as well; checked where the
    // backend offers listing.
    match b.prefix_list(&format!("{base}a/"), None, 100).await {
        Ok(p) => {
            let got: BTreeSet<String> = p.keys.into_iter().collect();
            if got.contains(&short) {
                return wrong("an expired entry is still listed");
            }
            if !got.contains(&live) {
                return wrong("the live sibling is missing from the listing");
            }
        }
        Err(KvError::Unsupported(_)) => {}
        Err(e) => return Err(e.into()),
    }
    Ok(())
}

async fn put_if_absent<K: KvBackend>(b: &K, base: &str) -> Result<(), Failure> {
    const CONTENDERS: usize = 16;
    const ROUNDS: usize = 20;
    let seq = format!("{base}seq");
    if !b.put_if_absent(&seq, b"one".to_vec(), None).await? {
        return wrong("put_if_absent on an absent key did not store");
    }
    if b.put_if_absent(&seq, b"two".to_vec(), None).await? {
        return wrong("put_if_absent on a present key stored");
    }
    if b.get(&seq).await?.as_deref() != Some(&b"one"[..]) {
        return wrong("a refused put_if_absent changed the value");
    }
    let payload = |i: usize| vec![i as u8; 1024];
    for round in 0..ROUNDS {
        let key = format!("{base}r{round}");
        let key = key.as_str();
        let tasks: Vec<Task<'_, Result<bool, KvError>>> = (0..CONTENDERS)
            .map(|i| -> Task<'_, _> { Box::pin(b.put_if_absent(key, payload(i), None)) })
            .collect();
        let mut winners = Vec::new();
        for (i, r) in join_all(tasks).await.into_iter().enumerate() {
            if r? {
                winners.push(i);
            }
        }
        if winners.len() != 1 {
            return wrong(format!(
                "round {round}: {} of {CONTENDERS} concurrent callers stored, want exactly 1",
                winners.len()
            ));
        }
        if b.get(key).await?.as_deref() != Some(payload(winners[0]).as_slice()) {
            return wrong(format!(
                "round {round}: the stored value is not the winner's"
            ));
        }
    }
    Ok(())
}

async fn seed_keys<K: KvBackend>(b: &K, base: &str) -> Result<(), Failure> {
    for k in [
        "a/1", "a/10", "a/2", "a/c/d", "a/a", "a/B", "ab", "abz", "a0x", "a%z", "a[0]x", "b/1",
    ] {
        b.put(&format!("{base}{k}"), k.as_bytes().to_vec(), None)
            .await?;
    }
    Ok(())
}

/// Every key under `prefix`, paging with `limit`, via `scan`; `Err` text if paging misbehaves.
async fn collect<F, Fut>(scan: F, limit: usize) -> Result<Vec<String>, Failure>
where
    F: Fn(Option<String>) -> Fut,
    Fut: std::future::Future<Output = Result<crate::KeyPage, KvError>>,
{
    let mut seen = Vec::new();
    let mut after: Option<String> = None;
    for _ in 0..100 {
        let p = scan(after.clone()).await?;
        if p.keys.len() > limit {
            return wrong(format!(
                "a page of limit {limit} held {} keys",
                p.keys.len()
            ));
        }
        seen.extend(p.keys);
        match p.next {
            Some(c) => after = Some(c),
            None => return Ok(seen),
        }
    }
    wrong("paging did not terminate in 100 pages")
}

async fn prefix_list<K: KvBackend>(b: &K, base: &str) -> Result<(), Failure> {
    seed_keys(b, base).await?;
    let full = |s: &str| format!("{base}{s}");
    let want: BTreeSet<String> = ["a/1", "a/10", "a/2", "a/B", "a/a", "a/c/d"]
        .iter()
        .map(|s| full(s))
        .collect();

    let got = collect(
        |after| {
            let p = full("a/");
            async move { b.prefix_list(&p, after.as_deref(), 2).await }
        },
        2,
    )
    .await?;
    let got: BTreeSet<String> = got.into_iter().collect();
    if got != want {
        return wrong(format!("prefix a/ listed {got:?}, want {want:?}"));
    }

    // The prefix is literal: `%` (SQL LIKE) and `[` (glob class) match only themselves.
    for (prefix, only) in [("a%", "a%z"), ("a[0]", "a[0]x")] {
        let p = b.prefix_list(&full(prefix), None, 100).await?;
        let got: BTreeSet<String> = p.keys.into_iter().collect();
        if got != BTreeSet::from([full(only)]) {
            return wrong(format!(
                "prefix {prefix} listed {got:?}, want only {:?}",
                full(only)
            ));
        }
    }

    // An expired key is not listed while its live sibling is.
    let gone = full("a/gone");
    b.put(&gone, b"x".to_vec(), Some(Duration::from_millis(100)))
        .await?;
    std::thread::sleep(Duration::from_millis(200));
    let p = b.prefix_list(&full("a/"), None, 100).await?;
    if p.keys.contains(&gone) {
        return wrong("an expired entry is listed");
    }
    if !p.keys.contains(&full("a/1")) {
        return wrong("a live key is missing next to an expired one");
    }
    Ok(())
}

async fn ordered_scan<K: KvBackend>(b: &K, base: &str) -> Result<(), Failure> {
    seed_keys(b, base).await?;
    let full = |s: &str| format!("{base}{s}");
    // "1" < "10" < "2" bytewise: a numeric or natural sort would put "2" before "10".
    // Bytewise: "1" < "10" < "2" < "B" (0x42) < "a" (0x61) < "c/d". A collation-aware or
    // case-insensitive order puts "a/a" before "a/B".
    let want: Vec<String> = ["a/1", "a/10", "a/2", "a/B", "a/a", "a/c/d"]
        .iter()
        .map(|s| full(s))
        .collect();
    let got = collect(
        |after| {
            let p = full("a/");
            async move { b.ordered_scan(&p, after.as_deref(), 2).await }
        },
        2,
    )
    .await?;
    if got != want {
        return wrong(format!("ordered scan returned {got:?}, want {want:?}"));
    }
    Ok(())
}

async fn compare_and_swap<K: KvBackend>(b: &K, base: &str) -> Result<(), Failure> {
    const CONTENDERS: usize = 16;
    const ROUNDS: usize = 20;
    let k = format!("{base}seq");
    b.put(&k, b"v1".to_vec(), None).await?;
    if !b.compare_and_swap(&k, b"v1", b"v2".to_vec(), None).await? {
        return wrong("a swap from the current value was refused");
    }
    if b.get(&k).await?.as_deref() != Some(&b"v2"[..]) {
        return wrong("a successful swap did not store the new value");
    }
    if b.compare_and_swap(&k, b"v1", b"v3".to_vec(), None).await? {
        return wrong("a swap from a stale value was accepted");
    }
    if b.get(&k).await?.as_deref() != Some(&b"v2"[..]) {
        return wrong("a refused swap changed the value");
    }
    let absent = format!("{base}absent");
    if b.compare_and_swap(&absent, b"", b"x".to_vec(), None)
        .await?
        || b.get(&absent).await?.is_some()
    {
        return wrong("a swap on an absent key created it");
    }
    for round in 0..ROUNDS {
        let key = format!("{base}r{round}");
        let key = key.as_str();
        b.put(key, b"0".to_vec(), None).await?;
        let tasks: Vec<Task<'_, Result<bool, KvError>>> = (0..CONTENDERS)
            .map(|i| -> Task<'_, _> {
                Box::pin(b.compare_and_swap(key, b"0", vec![b'a' + i as u8], None))
            })
            .collect();
        let mut winners = 0;
        for r in join_all(tasks).await {
            if r? {
                winners += 1;
            }
        }
        if winners != 1 {
            return wrong(format!(
                "round {round}: {winners} of {CONTENDERS} concurrent swaps from \"0\" won, want exactly 1"
            ));
        }
    }
    Ok(())
}

async fn compare_and_delete<K: KvBackend>(b: &K, base: &str) -> Result<(), Failure> {
    const CONTENDERS: usize = 16;
    const ROUNDS: usize = 20;
    let k = format!("{base}seq");
    b.put(&k, b"v".to_vec(), None).await?;
    if b.compare_and_delete(&k, b"x").await? {
        return wrong("a delete expecting a different value was accepted");
    }
    if b.get(&k).await?.is_none() {
        return wrong("a refused delete removed the value");
    }
    if !b.compare_and_delete(&k, b"v").await? {
        return wrong("a delete expecting the current value was refused");
    }
    if b.get(&k).await?.is_some() {
        return wrong("a successful delete left the value");
    }
    if b.compare_and_delete(&k, b"v").await? {
        return wrong("a delete on an absent key reported success");
    }
    for round in 0..ROUNDS {
        let key = format!("{base}r{round}");
        let key = key.as_str();
        b.put(key, b"v".to_vec(), None).await?;
        let tasks: Vec<Task<'_, Result<bool, KvError>>> = (0..CONTENDERS)
            .map(|_| -> Task<'_, _> { Box::pin(b.compare_and_delete(key, b"v")) })
            .collect();
        let mut winners = 0;
        for r in join_all(tasks).await {
            if r? {
                winners += 1;
            }
        }
        if winners != 1 {
            return wrong(format!(
                "round {round}: {winners} of {CONTENDERS} concurrent deletes won, want exactly 1"
            ));
        }
    }
    Ok(())
}

async fn ttl_remaining<K: KvBackend>(b: &K, base: &str) -> Result<(), Failure> {
    let ten = Duration::from_secs(10);
    let k = format!("{base}k");
    b.put(&k, b"v".to_vec(), Some(ten)).await?;
    let first = match b.ttl_remaining(&k).await? {
        Some(d) if !d.is_zero() && d <= ten => d,
        other => return wrong(format!("a 10 s entry reported {other:?} remaining")),
    };
    std::thread::sleep(Duration::from_millis(150));
    match b.ttl_remaining(&k).await? {
        Some(d) if d < first => {}
        other => {
            return wrong(format!(
                "remaining time did not fall after 150 ms: {first:?} then {other:?}"
            ));
        }
    }
    let forever = format!("{base}forever");
    b.put(&forever, b"v".to_vec(), None).await?;
    if b.ttl_remaining(&forever).await?.is_some() {
        return wrong("an entry written without a time to live reports one");
    }
    if b.ttl_remaining(&format!("{base}absent")).await?.is_some() {
        return wrong("an absent key reports a time to live");
    }
    let gone = format!("{base}gone");
    b.put(&gone, b"v".to_vec(), Some(Duration::from_millis(100)))
        .await?;
    std::thread::sleep(Duration::from_millis(250));
    if b.ttl_remaining(&gone).await?.is_some() {
        return wrong("an expired entry reports a time to live");
    }
    Ok(())
}

async fn purge_expired<K: KvBackend>(b: &K, base: &str) -> Result<(), Failure> {
    let short = Duration::from_millis(100);
    let (e1, e2) = (format!("{base}e1"), format!("{base}e2"));
    let live = format!("{base}live");
    let timed = format!("{base}timed");
    b.put(&e1, b"x".to_vec(), Some(short)).await?;
    b.put(&e2, b"x".to_vec(), Some(short)).await?;
    b.put(&live, b"l".to_vec(), None).await?;
    b.put(&timed, b"t".to_vec(), Some(Duration::from_secs(60)))
        .await?;
    std::thread::sleep(Duration::from_millis(250));
    let n = b.purge_expired().await?;
    if n < 2 {
        return wrong(format!(
            "two expired entries purged, but {n} reported removed"
        ));
    }
    if b.get(&live).await?.as_deref() != Some(&b"l"[..]) {
        return wrong("an entry without a time to live was removed by purge");
    }
    if b.get(&timed).await?.as_deref() != Some(&b"t"[..]) {
        return wrong("a live entry with a time to live was removed by purge");
    }
    if b.get(&e1).await?.is_some() || b.get(&e2).await?.is_some() {
        return wrong("an expired entry is visible after purge");
    }
    Ok(())
}

async fn batch_rollback<K: KvBackend>(b: &K, base: &str) -> Result<(), Failure> {
    const CONTENDERS: usize = 16;
    const ROUNDS: usize = 20;
    let (a, bk, c) = (format!("{base}a"), format!("{base}b"), format!("{base}c"));
    // A met precondition: everything applies together.
    b.put(&c, b"x".to_vec(), None).await?;
    let ok = b
        .batch(&[
            BatchOp::require(&c, Some(b"x".to_vec())),
            BatchOp::put(&a, b"1".to_vec()),
            BatchOp::put(&bk, b"2".to_vec()),
            BatchOp::delete(&c),
        ])
        .await?;
    if !ok
        || b.get(&a).await?.as_deref() != Some(&b"1"[..])
        || b.get(&bk).await?.as_deref() != Some(&b"2"[..])
        || b.get(&c).await?.is_some()
    {
        return wrong("a batch whose precondition held did not apply entirely");
    }
    // A failed precondition after earlier writes in the same batch: nothing changes.
    let refused = b
        .batch(&[
            BatchOp::put(&a, b"CHANGED".to_vec()),
            BatchOp::delete(&bk),
            BatchOp::put(format!("{base}new"), b"n".to_vec()),
            BatchOp::require(&c, Some(b"x".to_vec())),
        ])
        .await?;
    if refused {
        return wrong("a batch with an unmet precondition reported success");
    }
    if b.get(&a).await?.as_deref() != Some(&b"1"[..])
        || b.get(&bk).await?.as_deref() != Some(&b"2"[..])
        || b.get(&format!("{base}new")).await?.is_some()
    {
        return wrong("a refused batch changed something (earlier operations were not undone)");
    }
    // `None` means absent: it fails against an existing key and holds for a missing one.
    if b.batch(&[
        BatchOp::require(&a, None),
        BatchOp::put(format!("{base}z"), b"z".to_vec()),
    ])
    .await?
        || b.get(&format!("{base}z")).await?.is_some()
    {
        return wrong("a Require-absent on an existing key did not refuse the batch");
    }
    // Concurrent batches requiring the same value: exactly one applies.
    for round in 0..ROUNDS {
        let seq = format!("{base}seq{round}");
        let seq = seq.as_str();
        b.put(seq, b"0".to_vec(), None).await?;
        let queue: Vec<String> = (0..CONTENDERS)
            .map(|i| format!("{base}q{round}-{i}"))
            .collect();
        let tasks: Vec<Task<'_, Result<bool, KvError>>> = (0..CONTENDERS)
            .map(|i| -> Task<'_, _> {
                let q = queue[i].clone();
                Box::pin(async move {
                    b.batch(&[
                        BatchOp::require(seq, Some(b"0".to_vec())),
                        BatchOp::put(seq, vec![b'a' + i as u8]),
                        BatchOp::put(q, b"entry".to_vec()),
                    ])
                    .await
                })
            })
            .collect();
        let mut winners = Vec::new();
        for (i, r) in join_all(tasks).await.into_iter().enumerate() {
            if r? {
                winners.push(i);
            }
        }
        if winners.len() != 1 {
            return wrong(format!(
                "round {round}: {} of {CONTENDERS} batches requiring the same value applied",
                winners.len()
            ));
        }
        let entries = {
            let mut n = 0;
            for q in &queue {
                if b.get(q).await?.is_some() {
                    n += 1;
                }
            }
            n
        };
        if entries != 1 {
            return wrong(format!(
                "round {round}: {entries} queue entries written, want exactly 1"
            ));
        }
    }
    Ok(())
}

/// `kv.bounded_call`: the backend blocks a batch on a lock (`bounded_call_probe`) and this judges
/// what it saw. A backend with no timeout configured is `Unsupported`, which is "off, missing".
async fn bounded_call<K: KvBackend>(b: &K) -> Result<(), Failure> {
    if b.call_bound().is_none() {
        return Err(KvError::Unsupported("no call timeout configured".to_string()).into());
    }
    let p = b.bounded_call_probe().await?;
    if !p.finished {
        return wrong(format!(
            "a call blocked on a lock never returned (bound {:?}, waited {:?})",
            p.bound, p.elapsed
        ));
    }
    match &p.error {
        Some(KvError::TimedOut(_)) => {}
        Some(other) => {
            return wrong(format!(
                "the blocked call failed, but not with the timeout: {other}"
            ));
        }
        None => return wrong("the blocked call succeeded while its lock was held"),
    }
    let limit = p.bound * 3 + Duration::from_secs(2);
    if p.elapsed > limit {
        return wrong(format!(
            "the blocked call took {:?} against a bound of {:?}",
            p.elapsed, p.bound
        ));
    }
    if p.applied_after {
        return wrong("a batch cut off by the timeout was applied afterwards");
    }
    if !p.recovered {
        return wrong("the store did not work again once the block was gone");
    }
    Ok(())
}

async fn absent_race<K: KvBackend>(b: &K, base: &str) -> Result<(), Failure> {
    const CONTENDERS: usize = 8;
    const ROUNDS: usize = 10;
    const COMMITS: usize = 3;
    const RETRIES: usize = 10_000;
    for round in 0..ROUNDS {
        // The version key is absent at the start of every round: the first racers all `Require` it
        // absent, which no row lock can protect. Every commit also writes a key all of them write.
        let ver = format!("{base}ver{round}");
        let ver = ver.as_str();
        let shared = format!("{base}shared{round}");
        let shared = shared.as_str();
        let tasks: Vec<Task<'_, Result<Vec<u64>, Failure>>> = (0..CONTENDERS)
            .map(|_| -> Task<'_, _> {
                Box::pin(async move {
                    let mut got = Vec::new();
                    for _ in 0..COMMITS {
                        let mut attempts = 0;
                        loop {
                            attempts += 1;
                            if attempts > RETRIES {
                                return wrong("a commit was refused 10000 times");
                            }
                            let cur = b.get(ver).await?;
                            let n: u64 = match &cur {
                                Some(v) => std::str::from_utf8(v)
                                    .ok()
                                    .and_then(|t| t.parse().ok())
                                    .ok_or_else(|| {
                                        Failure::Wrong("the version is not a number".into())
                                    })?,
                                None => 0,
                            };
                            yield_now().await;
                            let next = (n + 1).to_string().into_bytes();
                            // An `Err` here is the defect: a racer that loses must be told `false`,
                            // not handed the server's abort.
                            if b.batch(&[
                                BatchOp::require(ver, cur),
                                BatchOp::put(shared, next.clone()),
                                BatchOp::put(ver, next),
                            ])
                            .await?
                            {
                                got.push(n + 1);
                                break;
                            }
                        }
                    }
                    Ok(got)
                })
            })
            .collect();
        let mut all = Vec::new();
        for r in join_all(tasks).await {
            all.extend(r?);
        }
        let distinct: BTreeSet<u64> = all.iter().copied().collect();
        if distinct.len() != all.len() {
            return wrong(format!(
                "round {round}: {} of {} version numbers were handed out twice",
                all.len() - distinct.len(),
                all.len()
            ));
        }
        let want = (CONTENDERS * COMMITS).to_string();
        let last = b.get(ver).await?.and_then(|v| String::from_utf8(v).ok());
        if last.as_deref() != Some(want.as_str()) {
            return wrong(format!("round {round}: ended at {last:?}, want {want}"));
        }
    }
    Ok(())
}

async fn batch_concurrent_read<K: KvBackend>(b: &K, base: &str) -> Result<(), Failure> {
    const READERS: usize = 4;
    const BATCHES: usize = 60;
    let (k1, k2) = (format!("{base}k1"), format!("{base}k2"));
    let (k1, k2) = (k1.as_str(), k2.as_str());
    if !b
        .batch(&[
            BatchOp::put(k1, b"0".to_vec()),
            BatchOp::put(k2, b"0".to_vec()),
        ])
        .await?
    {
        return wrong("a batch of two puts was refused");
    }
    let done = std::cell::Cell::new(false);
    let done = &done;
    let mut tasks: Vec<Task<'_, Result<(), Failure>>> = Vec::new();
    tasks.push(Box::pin(async move {
        for i in 1..=BATCHES {
            let v = i.to_string().into_bytes();
            let r = b
                .batch(&[BatchOp::put(k1, v.clone()), BatchOp::put(k2, v)])
                .await;
            match r {
                Ok(true) => {}
                Ok(false) => {
                    done.set(true);
                    return wrong("a batch of two puts was refused");
                }
                Err(e) => {
                    done.set(true);
                    return Err(e.into());
                }
            }
            yield_now().await;
        }
        done.set(true);
        Ok(())
    }));
    let prefix = base.to_string();
    for _ in 0..READERS {
        let prefix = prefix.clone();
        tasks.push(Box::pin(async move {
            for _ in 0..(BATCHES * 20) {
                let page = match b.ordered_scan_values(&prefix, None, 10).await {
                    Ok(p) => p,
                    Err(e) => {
                        done.set(true);
                        return Err(e.into());
                    }
                };
                let get = |k: &str| {
                    page.entries
                        .iter()
                        .find(|(key, _)| key == k)
                        .map(|(_, v)| v.clone())
                };
                if get(k1) != get(k2) {
                    done.set(true);
                    return wrong(format!(
                        "one scan saw a batch partly: k1={:?} k2={:?}",
                        get(k1).map(|v| String::from_utf8_lossy(&v).into_owned()),
                        get(k2).map(|v| String::from_utf8_lossy(&v).into_owned())
                    ));
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

/// The mailbox pattern: each caller reads the sequence, lets others run, then commits one batch
/// that requires the value it read. A refused batch is retried. Every caller must end with a
/// distinct number and the final counter must equal the number of increments.
///
/// The `yield_now` between the read and the commit widens the race window; without it a fast
/// backend can hide a missing `Require` (the control for this leaf is exactly that defect).
async fn optimistic_sequence<K: KvBackend>(b: &K, base: &str) -> Result<(), Failure> {
    const CONTENDERS: usize = 8;
    const ROUNDS: usize = 10;
    const RETRIES: usize = 10_000;
    let seq = format!("{base}seq");
    let seq = seq.as_str();
    b.put(seq, b"0".to_vec(), None).await?;
    let tasks: Vec<Task<'_, Result<Vec<u64>, Failure>>> = (0..CONTENDERS)
        .map(|_| -> Task<'_, _> {
            Box::pin(async move {
                let mut got = Vec::new();
                for _ in 0..ROUNDS {
                    let mut attempts = 0;
                    loop {
                        attempts += 1;
                        if attempts > RETRIES {
                            return wrong("an optimistic increment was refused 10000 times");
                        }
                        let cur = b
                            .get(seq)
                            .await?
                            .ok_or_else(|| Failure::Wrong("the sequence key vanished".into()))?;
                        let n: u64 = std::str::from_utf8(&cur)
                            .ok()
                            .and_then(|t| t.parse().ok())
                            .ok_or_else(|| Failure::Wrong("the sequence is not a number".into()))?;
                        yield_now().await;
                        let next = (n + 1).to_string().into_bytes();
                        if b.batch(&[BatchOp::require(seq, Some(cur)), BatchOp::put(seq, next)])
                            .await?
                        {
                            got.push(n + 1);
                            break;
                        }
                    }
                }
                Ok(got)
            })
        })
        .collect();
    let mut all = Vec::new();
    for r in join_all(tasks).await {
        all.extend(r?);
    }
    let total = (CONTENDERS * ROUNDS) as u64;
    let distinct: BTreeSet<u64> = all.iter().copied().collect();
    if distinct.len() != all.len() {
        return wrong(format!(
            "{} of {} sequence numbers were handed out twice (lost updates)",
            all.len() - distinct.len(),
            all.len()
        ));
    }
    let last = b.get(seq).await?.and_then(|v| String::from_utf8(v).ok());
    if last.as_deref() != Some(total.to_string().as_str()) {
        return wrong(format!("the sequence ended at {last:?}, want {total}"));
    }
    Ok(())
}

async fn scan_values<K: KvBackend>(b: &K, base: &str) -> Result<(), Failure> {
    let full = |s: &str| format!("{base}{s}");
    for k in ["s/2", "s/10", "s/1", "s/B", "s/a", "t/1"] {
        b.put(&full(k), format!("v:{k}").into_bytes(), None).await?;
    }
    let live = full("s/live");
    b.put(&live, b"keep".to_vec(), None).await?;
    let gone = full("s/gone");
    b.put(&gone, b"x".to_vec(), Some(Duration::from_millis(100)))
        .await?;
    std::thread::sleep(Duration::from_millis(250));
    let mut got: Vec<(String, Vec<u8>)> = Vec::new();
    let mut after: Option<String> = None;
    for _ in 0..50 {
        let p = b
            .ordered_scan_values(&full("s/"), after.as_deref(), 2)
            .await?;
        if p.entries.len() > 2 {
            return wrong(format!(
                "a page of limit 2 held {} entries",
                p.entries.len()
            ));
        }
        got.extend(p.entries);
        match p.next {
            Some(c) => after = Some(c),
            None => break,
        }
    }
    let keys: Vec<&str> = got.iter().map(|(k, _)| k.as_str()).collect();
    // Bytewise: "1" < "10" < "2" < "B" < "a" < "live".
    let want: Vec<String> = ["s/1", "s/10", "s/2", "s/B", "s/a", "s/live"]
        .iter()
        .map(|s| full(s))
        .collect();
    if keys != want.iter().map(String::as_str).collect::<Vec<_>>() {
        return wrong(format!(
            "scan with values returned keys {keys:?}, want {want:?}"
        ));
    }
    for (k, v) in &got {
        let suffix = k.strip_prefix(base).unwrap_or(k);
        let expect: Vec<u8> = if suffix == "s/live" {
            b"keep".to_vec()
        } else {
            format!("v:{suffix}").into_bytes()
        };
        if *v != expect {
            return wrong(format!("key {suffix} came back with value {v:?}"));
        }
    }
    Ok(())
}

async fn lease<K: KvBackend>(b: &K, base: &str) -> Result<(), Failure> {
    use crate::Lease;
    const CONTENDERS: usize = 16;
    const ROUNDS: usize = 5;
    let ttl = Duration::from_millis(200);
    let long = Duration::from_secs(30);
    let key = format!("{base}lease");

    let Some(a) = Lease::acquire(b, &key, "A", long).await? else {
        return wrong("a free lease was not granted");
    };
    if Lease::acquire(b, &key, "B", long).await?.is_some() {
        return wrong("a held lease was granted to a second owner");
    }
    if !a.renew(long).await? {
        return wrong("the owner could not renew its own lease");
    }
    if !a.release().await? {
        return wrong("the owner could not release its own lease");
    }
    let Some(b1) = Lease::acquire(b, &key, "B", long).await? else {
        return wrong("a released lease was not granted again");
    };
    b1.release().await?;

    // Expiry and safe reclaim.
    let Some(a) = Lease::acquire(b, &key, "A", ttl).await? else {
        return wrong("a free lease was not granted after release");
    };
    std::thread::sleep(Duration::from_millis(300));
    let Some(b2) = Lease::acquire(b, &key, "B", long).await? else {
        return wrong("an expired lease was not reclaimed");
    };
    if a.renew(long).await? {
        return wrong("a former owner renewed a lease that another owner now holds");
    }
    if a.release().await? {
        return wrong("a former owner released a lease that another owner now holds");
    }
    if Lease::acquire(b, &key, "C", long).await?.is_some() {
        return wrong("the former owner's release freed the new owner's lease");
    }
    b2.release().await?;

    // Concurrent reclaim of an expired lease: exactly one new owner per round.
    for round in 0..ROUNDS {
        let k = format!("{base}race{round}");
        let k = k.as_str();
        let Some(_old) = Lease::acquire(b, k, "old", ttl).await? else {
            return wrong("a free lease was not granted before a race round");
        };
        std::thread::sleep(Duration::from_millis(300));
        let names: Vec<String> = (0..CONTENDERS).map(|i| format!("c{i}")).collect();
        let tasks: Vec<Task<'_, Result<bool, KvError>>> = names
            .iter()
            .map(|n| -> Task<'_, _> {
                Box::pin(async move { Ok(Lease::acquire(b, k, n, long).await?.is_some()) })
            })
            .collect();
        let mut winners = 0;
        for r in join_all(tasks).await {
            if r? {
                winners += 1;
            }
        }
        if winners != 1 {
            return wrong(format!(
                "race {round}: {winners} of {CONTENDERS} reclaimed an expired lease, want exactly 1"
            ));
        }
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
/// A leaf is **on** only if the target passed *and* the positive control passed *and* every
/// known-bad control failed. `config` is the facts the gate can read for this target; `metadata`
/// is copied into every row with `test=kv::<leaf>` and the trial parameters added.
pub async fn certify<K: KvBackend>(
    target: &K,
    config: &Config,
    metadata: &Config,
) -> Vec<Certified> {
    certify_leaves(target, config, metadata, &Leaf::ALL).await
}

/// As [`certify`], for the listed leaves only (each with its controls).
pub async fn certify_leaves<K: KvBackend>(
    target: &K,
    config: &Config,
    metadata: &Config,
    leaves: &[Leaf],
) -> Vec<Certified> {
    let mut out = Vec::new();
    for leaf in leaves.iter().copied() {
        let reference = if leaf == Leaf::BoundedCall {
            MemoryKv::bounded()
        } else {
            MemoryKv::new()
        };
        let positive_passed = run_leaf(leaf, &reference).await.is_ok();
        let mut known_bad_failed = true;
        for d in leaf.defects() {
            if run_leaf(leaf, &MemoryKv::with_defect(*d)).await.is_ok() {
                known_bad_failed = false;
            }
        }
        let controls = Controls {
            positive_passed,
            known_bad_failed,
        };
        let result = run_leaf(leaf, target).await;
        // Whether a call bound is configured is a fact the gate can read at open, so it is part of
        // this leaf's row: a backend opened without a bound is off, the same backend opened with
        // one is judged on its own row, and neither row vetoes the other.
        let bound_cfg;
        let config = if leaf == Leaf::BoundedCall {
            bound_cfg = config.clone().with(
                "call_bound",
                if target.call_bound().is_some() {
                    "set"
                } else {
                    "unset"
                },
            );
            &bound_cfg
        } else {
            config
        };
        let mut md = metadata.clone().with("test", format!("kv::{}", leaf.id()));
        if let Ok(t) = Config::parse(leaf.trials()) {
            for key in ["contenders", "rounds", "value_bytes", "ttl_ms", "sleep_ms"] {
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
            (Err(Failure::Backend(KvError::Unsupported(m))), _) => {
                off(id, target, config, md, Reason::Missing, m.clone())
            }
            (Err(Failure::Backend(e)), _) => {
                off(id, target, config, md, Reason::Missing, e.to_string())
            }
        };
        out.push(Certified { leaf, row });
    }
    out
}

fn off<K: KvBackend>(
    subject: CapabilityId,
    target: &K,
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

    /// A backend whose first `ordered_scan_values` is wrong in the way a stepped server clock makes it
    /// (an expired entry still listed) and correct afterwards.
    struct StepsOnce {
        inner: MemoryKv,
        scans: std::sync::atomic::AtomicU32,
    }

    impl KvBackend for StepsOnce {
        fn backend(&self) -> &str {
            self.inner.backend()
        }
        fn version(&self) -> &str {
            self.inner.version()
        }
        async fn get(&self, key: &str) -> Result<Option<Vec<u8>>, KvError> {
            self.inner.get(key).await
        }
        async fn put(
            &self,
            key: &str,
            value: Vec<u8>,
            ttl: Option<Duration>,
        ) -> Result<(), KvError> {
            self.inner.put(key, value, ttl).await
        }
        async fn delete(&self, key: &str) -> Result<(), KvError> {
            self.inner.delete(key).await
        }
        async fn prefix_list(
            &self,
            prefix: &str,
            after: Option<&str>,
            limit: usize,
        ) -> Result<crate::KeyPage, KvError> {
            self.inner.prefix_list(prefix, after, limit).await
        }
        async fn ordered_scan_values(
            &self,
            prefix: &str,
            after: Option<&str>,
            limit: usize,
        ) -> Result<crate::ValuePage, KvError> {
            let first = self.scans.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0;
            if first {
                // The entry that expired is still there: bypass the expiry check.
                let mut page = self.inner.ordered_scan_values(prefix, after, limit).await?;
                page.entries.push((format!("{prefix}gone"), b"x".to_vec()));
                return Ok(page);
            }
            self.inner.ordered_scan_values(prefix, after, limit).await
        }
    }

    /// A backend on a machine that stalls once: the first `get` happens only after the pause (the
    /// way a paused VM or a starved CI runner delays it), so an entry with a 1 s window has expired
    /// by the time it is read. The backend itself is correct.
    struct StallsOnce {
        inner: MemoryKv,
        gets: std::sync::atomic::AtomicU32,
    }

    impl KvBackend for StallsOnce {
        fn backend(&self) -> &str {
            self.inner.backend()
        }
        fn version(&self) -> &str {
            self.inner.version()
        }
        async fn get(&self, key: &str) -> Result<Option<Vec<u8>>, KvError> {
            if self.gets.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                std::thread::sleep(Duration::from_millis(1100));
            }
            self.inner.get(key).await
        }
        async fn put(
            &self,
            key: &str,
            value: Vec<u8>,
            ttl: Option<Duration>,
        ) -> Result<(), KvError> {
            self.inner.put(key, value, ttl).await
        }
        async fn delete(&self, key: &str) -> Result<(), KvError> {
            self.inner.delete(key).await
        }
        async fn prefix_list(
            &self,
            prefix: &str,
            after: Option<&str>,
            limit: usize,
        ) -> Result<crate::KeyPage, KvError> {
            self.inner.prefix_list(prefix, after, limit).await
        }
    }

    /// A backend that acknowledges an entry with a time to live and never keeps it.
    struct DropsTtlEntries(MemoryKv);

    impl KvBackend for DropsTtlEntries {
        fn backend(&self) -> &str {
            self.0.backend()
        }
        fn version(&self) -> &str {
            self.0.version()
        }
        async fn get(&self, key: &str) -> Result<Option<Vec<u8>>, KvError> {
            self.0.get(key).await
        }
        async fn put(
            &self,
            key: &str,
            value: Vec<u8>,
            ttl: Option<Duration>,
        ) -> Result<(), KvError> {
            if ttl.is_some() {
                return Ok(());
            }
            self.0.put(key, value, ttl).await
        }
        async fn delete(&self, key: &str) -> Result<(), KvError> {
            self.0.delete(key).await
        }
    }

    #[tokio::test]
    async fn a_stall_past_the_ttl_window_is_not_a_verdict_but_a_miss_inside_it_is() {
        // The read came after the window had closed: inconclusive, so the check repeats with a
        // longer window and passes, in ONE attempt (no leaf-level retry needed).
        let stalled = StallsOnce {
            inner: MemoryKv::new(),
            gets: std::sync::atomic::AtomicU32::new(0),
        };
        assert_eq!(run_leaf_once(Leaf::Ttl, &stalled).await, Ok(()));
        // Positive control that the pause happened: more than one `get` of the short key.
        assert!(stalled.gets.load(std::sync::atomic::Ordering::SeqCst) >= 3);
        // A backend that loses the entry while the window is open fails at once.
        let bad = DropsTtlEntries(MemoryKv::new());
        assert!(matches!(
            run_leaf_once(Leaf::Ttl, &bad).await,
            Err(Failure::Wrong(_))
        ));
    }

    #[tokio::test]
    async fn a_clock_step_on_one_attempt_is_retried_and_a_defect_is_not() {
        let flaky = StepsOnce {
            inner: MemoryKv::new(),
            scans: std::sync::atomic::AtomicU32::new(0),
        };
        // One attempt sees the wrong page ...
        assert!(matches!(
            run_leaf_once(Leaf::ScanValues, &flaky).await,
            Err(Failure::Wrong(_))
        ));
        // ... and `run_leaf` retries it and passes on the second.
        let flaky = StepsOnce {
            inner: MemoryKv::new(),
            scans: std::sync::atomic::AtomicU32::new(0),
        };
        assert_eq!(run_leaf(Leaf::ScanValues, &flaky).await, Ok(()));
        // A real defect fails every attempt, so the retry cannot hide it.
        let bad = MemoryKv::with_defect(Defect::ScanIsDescending);
        assert!(matches!(
            run_leaf(Leaf::ScanValues, &bad).await,
            Err(Failure::Wrong(_))
        ));
    }
    use crate::KvOnBlob;
    use persistant_blob::{Defect as BlobDefect, MemoryBlob};

    fn state(rows: &[Certified], id: &str) -> Option<Reason> {
        let c = rows.iter().find(|c| c.leaf.id() == id).expect("row");
        if c.row.enabled { None } else { c.row.reason }
    }

    #[tokio::test]
    async fn a_correct_backend_is_on_for_every_leaf() {
        let rows = certify(&MemoryKv::bounded(), &Config::new(), &Config::new()).await;
        assert_eq!(rows.len(), Leaf::ALL.len());
        for c in &rows {
            assert!(c.row.enabled, "{}: {}", c.leaf.id(), c.row.note);
            assert!(c.row.metadata.get("test").is_some());
        }
    }

    #[tokio::test]
    async fn bounded_call_is_declared_only_when_a_timeout_is_configured() {
        // No timeout configured: the leaf is off as missing, never wrong, never on.
        let rows = certify_leaves(
            &MemoryKv::new(),
            &Config::new(),
            &Config::new(),
            &[Leaf::BoundedCall],
        )
        .await;
        assert_eq!(state(&rows, "kv.bounded_call"), Some(Reason::Missing));
        // A timeout configured and held: on.
        let rows = certify_leaves(
            &MemoryKv::bounded(),
            &Config::new(),
            &Config::new(),
            &[Leaf::BoundedCall],
        )
        .await;
        assert_eq!(state(&rows, "kv.bounded_call"), None);
        // A timeout configured and broken, either way: wrong.
        for d in [Defect::BoundIgnored, Defect::TimeoutLeavesBatchApplied] {
            assert!(matches!(
                run_leaf(Leaf::BoundedCall, &MemoryKv::with_defect(d)).await,
                Err(Failure::Wrong(_))
            ));
        }
    }

    #[tokio::test]
    async fn every_defect_turns_its_leaf_off_as_wrong() {
        // Run the leaf directly: a full `certify` per defect would repeat every other leaf's
        // real-time waits.
        for leaf in Leaf::ALL {
            for d in leaf.defects() {
                let r = run_leaf(leaf, &MemoryKv::with_defect(*d)).await;
                assert!(
                    matches!(r, Err(Failure::Wrong(_))),
                    "{d:?} vs {}: {r:?}",
                    leaf.id()
                );
            }
        }
    }

    #[tokio::test]
    async fn the_harness_can_tell_leaves_apart() {
        let rows = certify(
            &MemoryKv::with_defect(Defect::DeleteIsIgnored),
            &Config::new(),
            &Config::new(),
        )
        .await;
        assert_eq!(state(&rows, "kv.delete"), Some(Reason::Wrong));
        assert_eq!(
            state(&rows, "kv.get"),
            None,
            "a delete defect must not turn get off"
        );
    }

    #[tokio::test]
    async fn kv_on_a_correct_blob_store_gives_what_the_blob_gives() {
        let kv = KvOnBlob::new(MemoryBlob::new());
        let rows = certify(&kv, &Config::new(), &Config::new()).await;
        for c in &rows {
            // A blob store has no compare-and-delete, so these are missing, not wrong.
            let needs_cad = matches!(
                c.leaf,
                Leaf::CompareAndSwap
                    | Leaf::CompareAndDelete
                    | Leaf::Lease
                    | Leaf::PurgeExpired
                    | Leaf::BatchRollback
                    | Leaf::BatchConcurrentRead
                    | Leaf::ScanValues
                    | Leaf::OptimisticSequence
                    | Leaf::AbsentRace
                    | Leaf::BoundedCall
            );
            assert_eq!(c.row.enabled, !needs_cad, "{}: {}", c.leaf.id(), c.row.note);
            if needs_cad {
                assert_eq!(c.row.reason, Some(Reason::Missing), "{}", c.leaf.id());
            }
        }
        assert_eq!(rows[0].row.backend, "kv-on-blob:memory");
    }

    #[tokio::test]
    async fn kv_on_blob_inherits_a_blob_defect_as_a_wrong_leaf() {
        // Control: the blob store lists in descending order, so the KV scan is wrong too.
        let kv = KvOnBlob::new(MemoryBlob::with_defect(BlobDefect::ListIsDescending));
        let rows = certify(&kv, &Config::new(), &Config::new()).await;
        assert_eq!(state(&rows, "kv.ordered_scan"), Some(Reason::Wrong));
        assert_eq!(state(&rows, "kv.get"), None);
    }

    #[tokio::test]
    async fn kv_on_blob_without_exclusive_create_is_missing_not_wrong() {
        struct NoExclusive(MemoryBlob);
        impl persistant_blob::BlobBackend for NoExclusive {
            fn backend(&self) -> &str {
                "no-exclusive"
            }
            fn version(&self) -> &str {
                "0"
            }
            async fn read(&self, k: &str) -> Result<Option<Vec<u8>>, persistant_blob::BlobError> {
                self.0.read(k).await
            }
            async fn write(&self, k: &str, v: Vec<u8>) -> Result<(), persistant_blob::BlobError> {
                self.0.write(k, v).await
            }
            async fn delete(&self, k: &str) -> Result<(), persistant_blob::BlobError> {
                self.0.delete(k).await
            }
            async fn stat(
                &self,
                k: &str,
            ) -> Result<Option<persistant_blob::Entry>, persistant_blob::BlobError> {
                self.0.stat(k).await
            }
            async fn list(
                &self,
                p: &str,
                a: Option<&str>,
                l: usize,
            ) -> Result<persistant_blob::Page, persistant_blob::BlobError> {
                self.0.list(p, a, l).await
            }
        }
        let kv = KvOnBlob::with_in_place_put(NoExclusive(MemoryBlob::new()));
        let rows = certify(&kv, &Config::new(), &Config::new()).await;
        assert_eq!(state(&rows, "kv.put_if_absent"), Some(Reason::Missing));
        assert_eq!(state(&rows, "kv.put"), None, "in-place put was chosen");
        assert_eq!(state(&rows, "kv.ttl"), None);
    }

    #[tokio::test]
    async fn strict_kv_on_a_blob_without_replace_atomic_refuses_put() {
        // Control for the in-place option: the same blob, strict, gives no `kv.put`.
        struct NoReplace(MemoryBlob);
        impl persistant_blob::BlobBackend for NoReplace {
            fn backend(&self) -> &str {
                "no-replace"
            }
            fn version(&self) -> &str {
                "0"
            }
            async fn read(&self, k: &str) -> Result<Option<Vec<u8>>, persistant_blob::BlobError> {
                self.0.read(k).await
            }
            async fn write(&self, k: &str, v: Vec<u8>) -> Result<(), persistant_blob::BlobError> {
                self.0.write(k, v).await
            }
            async fn delete(&self, k: &str) -> Result<(), persistant_blob::BlobError> {
                self.0.delete(k).await
            }
            async fn stat(
                &self,
                k: &str,
            ) -> Result<Option<persistant_blob::Entry>, persistant_blob::BlobError> {
                self.0.stat(k).await
            }
            async fn list(
                &self,
                p: &str,
                a: Option<&str>,
                l: usize,
            ) -> Result<persistant_blob::Page, persistant_blob::BlobError> {
                self.0.list(p, a, l).await
            }
        }
        let kv = KvOnBlob::new(NoReplace(MemoryBlob::new()));
        let rows = certify(&kv, &kv.config(), &Config::new()).await;
        assert_eq!(state(&rows, "kv.put"), Some(Reason::Missing));
        assert_eq!(rows[0].row.config.get("put_mode"), Some("atomic"));
    }
}
