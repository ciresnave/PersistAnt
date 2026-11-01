// SPDX-License-Identifier: MIT OR Apache-2.0
//! Copy every pair of one key-value store into another, read it back, and say how far it got.
//!
//! [`copy_all`] is the copy; [`copy_all_with_progress`] is the same with a callback that hears of
//! each pair as it is handled, in pairs (not bytes), so a front end can draw a bar or a count where
//! it had only a spinner. The source is only read. A target that already holds keys is refused unless
//! [`CopyOptions::allow_existing`], and then no key of the target is overwritten. The result is read
//! back before `Ok`: the source is scanned a second time (not remembered) and every pair compared
//! with what the target now holds, so a target that acknowledged a write and lost it is an error.
//!
//! The callback runs inline between awaits and must be quick; it cannot cancel the copy. Errors keep
//! the backend's own [`KvError`] untouched, so a caller that holds a secret (a URL with a password)
//! scrubs the text itself.

use crate::{KvBackend, KvError};
use std::collections::HashSet;

/// Which store an error came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    /// The store being copied from.
    Source,
    /// The store being copied into.
    Target,
}

/// What [`copy_all`] was asked to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopyOptions {
    /// Copy into a target that already holds keys, keeping every key it has (default `false`: a
    /// non-empty target is refused).
    pub allow_existing: bool,
    /// Pairs per page read from the source (default 200, at least 1).
    pub page_size: usize,
    /// Count the source's keys first, so [`Progress::total`] is known (default `false`: one extra
    /// key-only scan, and a source that changes while it is copied can end up with another total).
    pub count_first: bool,
}

impl Default for CopyOptions {
    fn default() -> Self {
        CopyOptions {
            allow_existing: false,
            page_size: 200,
            count_first: false,
        }
    }
}

/// The part of the work a [`Progress`] report is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Pairs are being written to the target.
    Copy,
    /// The copy is being read back and compared.
    Verify,
}

/// How far the copy has got: pairs of the source handled so far in this phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Progress {
    /// What is being done.
    pub phase: Phase,
    /// Source pairs handled in this phase: copied or, with `allow_existing`, found already there.
    /// It is 0 in the first report of a phase.
    pub done: u64,
    /// The number of keys the source had when the copy began, if [`CopyOptions::count_first`].
    pub total: Option<u64>,
}

/// What a finished copy did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopyReport {
    /// Pairs written to the target.
    pub copied: u64,
    /// Of those, pairs written with a time to live.
    pub with_ttl: u64,
    /// Pairs not written because the target already had the key (`allow_existing`).
    pub skipped: u64,
    /// Pairs read back from the target and found equal.
    pub verified: u64,
}

/// Why a copy did not finish.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum CopyError {
    /// The target holds keys and [`CopyOptions::allow_existing`] is off. Nothing was written.
    TargetNotEmpty,
    /// A backend call failed.
    Kv {
        /// Which store.
        side: Side,
        /// The operation that failed.
        op: &'static str,
        /// The backend's error, as it gave it.
        error: KvError,
    },
    /// The read-back found the target different from the source.
    Mismatch {
        /// Source keys the target does not have.
        missing: u64,
        /// Source keys the target holds with another value.
        differing: u64,
        /// Target keys beyond the source's (only checked without `allow_existing`).
        extra: u64,
        /// Up to ten keys that were missing or differing.
        sample: Vec<String>,
    },
}

impl std::fmt::Display for CopyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CopyError::TargetNotEmpty => write!(f, "the target already holds keys"),
            CopyError::Kv { side, op, error } => {
                let side = match side {
                    Side::Source => "source",
                    Side::Target => "target",
                };
                write!(f, "{side} {op}: {error}")
            }
            CopyError::Mismatch {
                missing,
                differing,
                extra,
                ..
            } => write!(
                f,
                "the target differs from the source: {missing} missing, {differing} differing, {extra} extra"
            ),
        }
    }
}

impl std::error::Error for CopyError {}

fn kv(side: Side, op: &'static str) -> impl FnOnce(KvError) -> CopyError {
    move |error| CopyError::Kv { side, op, error }
}

/// One page of the source as `(key, value)` pairs and the cursor for the next. A backend that scans
/// keys and values together in one snapshot (`ordered_scan_values`) does; the others scan keys and
/// `get` each value (a key that expired in between is skipped).
async fn read_page<S: KvBackend>(
    source: &S,
    after: Option<&str>,
    limit: usize,
) -> Result<(Vec<(String, Vec<u8>)>, Option<String>), CopyError> {
    match source.ordered_scan_values("", after, limit).await {
        Ok(page) => return Ok((page.entries, page.next)),
        Err(KvError::Unsupported(_)) => {}
        Err(e) => return Err(kv(Side::Source, "ordered_scan_values")(e)),
    }
    let keys = source
        .ordered_scan("", after, limit)
        .await
        .map_err(kv(Side::Source, "ordered_scan"))?;
    let mut entries = Vec::with_capacity(keys.keys.len());
    for key in keys.keys {
        if let Some(value) = source.get(&key).await.map_err(kv(Side::Source, "get"))? {
            entries.push((key, value));
        }
    }
    Ok((entries, keys.next))
}

async fn target_has_any<T: KvBackend>(target: &T) -> Result<bool, CopyError> {
    match target.ordered_scan("", None, 1).await {
        Ok(page) => Ok(!page.keys.is_empty()),
        Err(KvError::Unsupported(_)) => target
            .prefix_list("", None, 1)
            .await
            .map(|page| !page.keys.is_empty())
            .map_err(kv(Side::Target, "prefix_list")),
        Err(e) => Err(kv(Side::Target, "ordered_scan")(e)),
    }
}

/// The number of keys, by paging a key-only scan.
async fn count_keys<B: KvBackend>(b: &B, side: Side, limit: usize) -> Result<u64, CopyError> {
    let (mut n, mut after) = (0u64, None::<String>);
    loop {
        let page = b
            .ordered_scan("", after.as_deref(), limit)
            .await
            .map_err(kv(side, "ordered_scan"))?;
        n += page.keys.len() as u64;
        match page.next {
            Some(cursor) => after = Some(cursor),
            None => return Ok(n),
        }
    }
}

/// Copy every pair of `source` into `target` and read the result back. See the module docs.
pub async fn copy_all<S: KvBackend, T: KvBackend>(
    source: &S,
    target: &T,
    options: &CopyOptions,
) -> Result<CopyReport, CopyError> {
    copy_all_with_progress(source, target, options, &mut |_| {}).await
}

/// As [`copy_all`], calling `progress` with a [`Progress`] at the start of each phase (`done` = 0)
/// and after every source pair handled in it. The last [`Phase::Copy`] report has `done` equal to
/// the pairs handled; the last [`Phase::Verify`] report is sent before the verdict, so a copy
/// that ends in [`CopyError::Mismatch`] has still reported its verify pass.
pub async fn copy_all_with_progress<S: KvBackend, T: KvBackend>(
    source: &S,
    target: &T,
    options: &CopyOptions,
    progress: &mut dyn FnMut(Progress),
) -> Result<CopyReport, CopyError> {
    let limit = options.page_size.max(1);
    if !options.allow_existing && target_has_any(target).await? {
        return Err(CopyError::TargetNotEmpty);
    }
    let total = if options.count_first {
        Some(count_keys(source, Side::Source, limit).await?)
    } else {
        None
    };

    let mut report = |phase: Phase, done: u64| progress(Progress { phase, done, total });
    let (mut copied, mut with_ttl, mut skipped) = (0u64, 0u64, 0u64);
    let mut kept: HashSet<String> = HashSet::new();
    let mut after: Option<String> = None;
    report(Phase::Copy, 0);
    loop {
        let (entries, next) = read_page(source, after.as_deref(), limit).await?;
        for (key, value) in entries {
            let ttl = source
                .ttl_remaining(&key)
                .await
                .map_err(kv(Side::Source, "ttl_remaining"))?;
            if options.allow_existing {
                let stored = target
                    .put_if_absent(&key, value, ttl)
                    .await
                    .map_err(kv(Side::Target, "put_if_absent"))?;
                if stored {
                    copied += 1;
                    with_ttl += u64::from(ttl.is_some());
                } else {
                    skipped += 1;
                    kept.insert(key);
                }
            } else {
                target
                    .put(&key, value, ttl)
                    .await
                    .map_err(kv(Side::Target, "put"))?;
                copied += 1;
                with_ttl += u64::from(ttl.is_some());
            }
            report(Phase::Copy, copied + skipped);
        }
        match next {
            Some(cursor) => after = Some(cursor),
            None => break,
        }
    }

    // Read both sides back. The source is scanned again, so what is compared is what is stored now.
    let (mut verified, mut missing, mut differing, mut seen) = (0u64, 0u64, 0u64, 0u64);
    let mut sample: Vec<String> = Vec::new();
    let mut after: Option<String> = None;
    report(Phase::Verify, 0);
    loop {
        let (entries, next) = read_page(source, after.as_deref(), limit).await?;
        for (key, value) in entries {
            seen += 1;
            if !kept.contains(&key) {
                let found = target.get(&key).await.map_err(kv(Side::Target, "get"))?;
                match found {
                    Some(stored) if stored == value => verified += 1,
                    other => {
                        if other.is_none() {
                            missing += 1;
                        } else {
                            differing += 1;
                        }
                        if sample.len() < 10 {
                            sample.push(key);
                        }
                    }
                }
            }
            report(Phase::Verify, seen);
        }
        match next {
            Some(cursor) => after = Some(cursor),
            None => break,
        }
    }
    let extra = if options.allow_existing {
        0
    } else {
        count_keys(target, Side::Target, limit)
            .await?
            .saturating_sub(verified + differing)
    };
    if missing + differing + extra > 0 {
        return Err(CopyError::Mismatch {
            missing,
            differing,
            extra,
            sample,
        });
    }
    Ok(CopyReport {
        copied,
        with_ttl,
        skipped,
        verified,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Defect, MemoryKv};
    use std::time::Duration;

    async fn filled(n: usize) -> MemoryKv {
        let s = MemoryKv::new();
        for i in 0..n {
            s.put(&format!("k{i:03}"), format!("v{i}").into_bytes(), None)
                .await
                .unwrap();
        }
        s
    }

    fn small(count_first: bool) -> CopyOptions {
        CopyOptions {
            page_size: 2,
            count_first,
            ..CopyOptions::default()
        }
    }

    #[tokio::test]
    async fn copies_every_pair_and_reads_it_back() {
        let (s, t) = (filled(5).await, MemoryKv::new());
        let r = copy_all(&s, &t, &small(false)).await.unwrap();
        assert_eq!((r.copied, r.skipped, r.verified, r.with_ttl), (5, 0, 5, 0));
        assert_eq!(t.get("k004").await.unwrap(), Some(b"v4".to_vec()));
    }

    #[tokio::test]
    async fn progress_counts_pairs_in_each_phase_and_knows_the_total_when_asked() {
        let (s, t) = (filled(5).await, MemoryKv::new());
        let mut seen: Vec<Progress> = Vec::new();
        copy_all_with_progress(&s, &t, &small(true), &mut |p| seen.push(p))
            .await
            .unwrap();
        let of = |ph: Phase| -> Vec<u64> {
            seen.iter()
                .filter(|p| p.phase == ph)
                .map(|p| p.done)
                .collect()
        };
        // Starts at 0, then one report per pair, in order, ending at the pair count.
        assert_eq!(of(Phase::Copy), vec![0, 1, 2, 3, 4, 5]);
        assert_eq!(of(Phase::Verify), vec![0, 1, 2, 3, 4, 5]);
        assert!(seen.iter().all(|p| p.total == Some(5)));
        // Copy is reported before verify.
        let first_verify = seen.iter().position(|p| p.phase == Phase::Verify).unwrap();
        assert!(seen[..first_verify].iter().all(|p| p.phase == Phase::Copy));
        // The control: without count_first the total is unknown, not 0.
        let mut plain: Vec<Progress> = Vec::new();
        copy_all_with_progress(&s, &MemoryKv::new(), &small(false), &mut |p| plain.push(p))
            .await
            .unwrap();
        assert!(plain.iter().all(|p| p.total.is_none()));
    }

    #[tokio::test]
    async fn a_non_empty_target_is_refused_and_untouched_unless_allowed() {
        let (s, t) = (filled(3).await, MemoryKv::new());
        t.put("k001", b"mine".to_vec(), None).await.unwrap();
        t.put("other", b"x".to_vec(), None).await.unwrap();
        assert_eq!(
            copy_all(&s, &t, &small(false)).await,
            Err(CopyError::TargetNotEmpty)
        );
        assert_eq!(t.get("k000").await.unwrap(), None, "nothing was written");
        let allow = CopyOptions {
            allow_existing: true,
            ..small(false)
        };
        let mut last = None;
        let r = copy_all_with_progress(&s, &t, &allow, &mut |p| last = Some(p))
            .await
            .unwrap();
        assert_eq!((r.copied, r.skipped, r.verified), (2, 1, 2));
        assert_eq!(t.get("k001").await.unwrap(), Some(b"mine".to_vec()), "kept");
        // Skipped pairs count as handled.
        assert_eq!(last.map(|p| (p.phase, p.done)), Some((Phase::Verify, 3)));
    }

    #[tokio::test]
    async fn a_time_to_live_travels_with_the_pair() {
        let s = MemoryKv::new();
        s.put("a", b"1".to_vec(), Some(Duration::from_secs(3600)))
            .await
            .unwrap();
        s.put("b", b"2".to_vec(), None).await.unwrap();
        let t = MemoryKv::new();
        let r = copy_all(&s, &t, &CopyOptions::default()).await.unwrap();
        assert_eq!((r.copied, r.with_ttl), (2, 1));
        assert!(t.ttl_remaining("a").await.unwrap().is_some());
        assert!(t.ttl_remaining("b").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn a_target_that_loses_writes_is_an_error_and_its_verify_pass_was_reported() {
        // The known-bad control: `PutIsLost` acknowledges a put and stores nothing.
        let (s, t) = (filled(4).await, MemoryKv::with_defect(Defect::PutIsLost));
        let mut verify_done = 0;
        let e = copy_all_with_progress(&s, &t, &small(false), &mut |p| {
            if p.phase == Phase::Verify {
                verify_done = p.done;
            }
        })
        .await
        .unwrap_err();
        match e {
            CopyError::Mismatch {
                missing, sample, ..
            } => {
                assert_eq!(missing, 4);
                assert_eq!(sample.len(), 4);
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(verify_done, 4);
    }

    #[tokio::test]
    async fn a_blob_backed_source_is_read_key_by_key() {
        // `KvOnBlob` over a blob store scans keys only, so each value is read with `get`.
        let s = crate::KvOnBlob::new(persistant_blob::MemoryBlob::new());
        for i in 0..5 {
            s.put(&format!("k{i}"), vec![i as u8; 3], None)
                .await
                .unwrap();
        }
        assert!(
            matches!(
                s.ordered_scan_values("", None, 10).await,
                Err(KvError::Unsupported(_))
            ),
            "control: this source has no value scan, so the fallback is what runs"
        );
        let t = MemoryKv::new();
        let r = copy_all(&s, &t, &small(true)).await.unwrap();
        assert_eq!((r.copied, r.verified), (5, 5));
        assert_eq!(t.get("k3").await.unwrap(), Some(vec![3; 3]));
    }

    #[tokio::test]
    async fn an_empty_source_copies_nothing_and_reports_zero() {
        let mut seen = Vec::new();
        let r =
            copy_all_with_progress(&MemoryKv::new(), &MemoryKv::new(), &small(true), &mut |p| {
                seen.push((p.phase, p.done, p.total))
            })
            .await
            .unwrap();
        assert_eq!((r.copied, r.verified), (0, 0));
        assert_eq!(
            seen,
            vec![(Phase::Copy, 0, Some(0)), (Phase::Verify, 0, Some(0))]
        );
    }
}
