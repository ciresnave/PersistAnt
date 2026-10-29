// SPDX-License-Identifier: MIT OR Apache-2.0
//! Stage 2 of backend benchmarking for the blob kind: **precision and per-operation statistics**.
//! The same shape as `persistant_kv::stats`. No timing (stage 3) and no ranking.
//!
//! [`measure`] runs each leaf's test `runs` times, one attempt each, and reports how often it passed,
//! how often the backend broke the contract and how often it errored, with the first failure's text.
//! [`Instrumented`] counts, per operation, the calls and the errors by kind. A leaf is **exact** only
//! if every run passed; a fraction is never rounded up, and the contract (stage 1) still decides
//! eligibility.

use crate::conformance::{Failure, Leaf, run_leaf};
use crate::{BlobBackend, BlobError, Entry, Page};
use std::collections::BTreeMap;
use std::sync::Mutex;

/// What one operation did while a test ran: calls, successes and errors by kind.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OpCount {
    /// Times it was called.
    pub calls: u64,
    /// Times it returned `Ok`.
    pub ok: u64,
    /// `BlobError::Backend` results.
    pub backend: u64,
    /// `BlobError::Unsupported` results.
    pub unsupported: u64,
}

impl OpCount {
    /// Every error, whatever its kind.
    pub fn errors(&self) -> u64 {
        self.backend + self.unsupported
    }
}

/// A [`BlobBackend`] that counts what the wrapped backend does, per operation. It changes nothing.
#[derive(Debug)]
pub struct Instrumented<'a, B> {
    inner: &'a B,
    counts: Mutex<BTreeMap<&'static str, OpCount>>,
}

impl<'a, B: BlobBackend> Instrumented<'a, B> {
    /// Wrap `inner`.
    pub fn new(inner: &'a B) -> Self {
        Instrumented {
            inner,
            counts: Mutex::new(BTreeMap::new()),
        }
    }

    /// The counts so far, by operation name (`read`, `write`, `append`, ...).
    pub fn counts(&self) -> BTreeMap<&'static str, OpCount> {
        self.counts.lock().map(|c| c.clone()).unwrap_or_default()
    }

    fn note<T>(&self, op: &'static str, r: Result<T, BlobError>) -> Result<T, BlobError> {
        if let Ok(mut c) = self.counts.lock() {
            let e = c.entry(op).or_default();
            e.calls += 1;
            match &r {
                Ok(_) => e.ok += 1,
                Err(BlobError::Backend(_)) => e.backend += 1,
                Err(BlobError::Unsupported(_)) => e.unsupported += 1,
                Err(BlobError::Changed(_)) => e.backend += 1,
            }
        }
        r
    }
}

impl<B: BlobBackend> BlobBackend for Instrumented<'_, B> {
    fn backend(&self) -> &str {
        self.inner.backend()
    }

    fn version(&self) -> &str {
        self.inner.version()
    }

    fn key_layout(&self) -> crate::KeyLayout {
        self.inner.key_layout()
    }

    async fn read(&self, key: &str) -> Result<Option<Vec<u8>>, BlobError> {
        self.note("read", self.inner.read(key).await)
    }

    async fn write(&self, key: &str, bytes: Vec<u8>) -> Result<(), BlobError> {
        self.note("write", self.inner.write(key, bytes).await)
    }

    async fn delete(&self, key: &str) -> Result<(), BlobError> {
        self.note("delete", self.inner.delete(key).await)
    }

    async fn stat(&self, key: &str) -> Result<Option<Entry>, BlobError> {
        self.note("stat", self.inner.stat(key).await)
    }

    async fn list(
        &self,
        prefix: &str,
        after: Option<&str>,
        limit: usize,
    ) -> Result<Page, BlobError> {
        self.note("list", self.inner.list(prefix, after, limit).await)
    }

    async fn create_exclusive(&self, key: &str, bytes: Vec<u8>) -> Result<bool, BlobError> {
        self.note(
            "create_exclusive",
            self.inner.create_exclusive(key, bytes).await,
        )
    }

    async fn replace_atomic(&self, key: &str, bytes: Vec<u8>) -> Result<(), BlobError> {
        self.note(
            "replace_atomic",
            self.inner.replace_atomic(key, bytes).await,
        )
    }

    async fn append(&self, key: &str, bytes: Vec<u8>) -> Result<(), BlobError> {
        self.note("append", self.inner.append(key, bytes).await)
    }

    async fn read_range(
        &self,
        key: &str,
        offset: u64,
        len: u64,
    ) -> Result<Option<Vec<u8>>, BlobError> {
        self.note("read_range", self.inner.read_range(key, offset, len).await)
    }

    async fn read_range_if(
        &self,
        key: &str,
        offset: u64,
        len: u64,
        tag: &str,
    ) -> Result<Option<Vec<u8>>, BlobError> {
        self.note(
            "read_range_if",
            self.inner.read_range_if(key, offset, len, tag).await,
        )
    }

    async fn write_stream(&self, key: &str) -> Result<Box<dyn crate::BlobSink>, BlobError> {
        self.note("write_stream", self.inner.write_stream(key).await)
    }

    async fn write_stream_atomic(&self, key: &str) -> Result<Box<dyn crate::BlobSink>, BlobError> {
        self.note(
            "write_stream_atomic",
            self.inner.write_stream_atomic(key).await,
        )
    }
}

/// The result of running one leaf's test `runs` times on a target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeafStats {
    /// The leaf.
    pub leaf: Leaf,
    /// Times the test ran.
    pub runs: u32,
    /// Runs that passed.
    pub passed: u32,
    /// Runs where the backend returned something the contract forbids.
    pub wrong: u32,
    /// Runs where the backend errored (including `Unsupported`) before the test could decide.
    pub errored: u32,
    /// The first failing run's text, if any.
    pub first_failure: Option<String>,
    /// What each operation did across all the runs.
    pub ops: BTreeMap<&'static str, OpCount>,
}

impl LeafStats {
    /// `true` only if every run passed. One failure in a thousand is not exact.
    pub fn is_exact(&self) -> bool {
        self.runs > 0 && self.passed == self.runs
    }

    /// Passed runs over runs, `0.0` for no runs. For reading; use [`is_exact`](Self::is_exact) to
    /// decide.
    pub fn precision(&self) -> f64 {
        if self.runs == 0 {
            0.0
        } else {
            f64::from(self.passed) / f64::from(self.runs)
        }
    }
}

/// Run each leaf's test `runs` times on `target` (one attempt per run) and count.
pub async fn measure<B: BlobBackend>(target: &B, leaves: &[Leaf], runs: u32) -> Vec<LeafStats> {
    let mut out = Vec::with_capacity(leaves.len());
    for leaf in leaves.iter().copied() {
        let counted = Instrumented::new(target);
        let (mut passed, mut wrong, mut errored) = (0, 0, 0);
        let mut first_failure = None;
        for _ in 0..runs {
            match run_leaf(leaf, &counted).await {
                Ok(()) => passed += 1,
                Err(Failure::Wrong(m)) => {
                    wrong += 1;
                    first_failure.get_or_insert(m);
                }
                Err(Failure::Backend(e)) => {
                    errored += 1;
                    first_failure.get_or_insert(e.to_string());
                }
            }
        }
        out.push(LeafStats {
            leaf,
            runs,
            passed,
            wrong,
            errored,
            first_failure,
            ops: counted.counts(),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Defect, MemoryBlob};

    #[tokio::test]
    async fn a_correct_backend_is_exact_on_every_run_and_the_counts_add_up() {
        let s = measure(&MemoryBlob::new(), &[Leaf::Read, Leaf::CreateExclusive], 3).await;
        for l in &s {
            assert!(l.is_exact(), "{l:?}");
            assert_eq!(l.precision(), 1.0);
            assert_eq!((l.wrong, l.errored), (0, 0));
        }
        let w = &s[0].ops["write"];
        assert_eq!(w.calls, w.ok + w.errors());
        assert!(w.calls >= 3, "the test writes on every run");
    }

    #[tokio::test]
    async fn a_defect_is_counted_as_wrong_not_rounded_to_a_pass() {
        let b = MemoryBlob::with_defect(Defect::DeleteIsIgnored);
        let s = measure(&b, &[Leaf::Delete], 4).await;
        assert_eq!((s[0].passed, s[0].wrong, s[0].errored), (0, 4, 0));
        assert!(!s[0].is_exact());
        assert!(s[0].first_failure.is_some());
    }

    #[tokio::test]
    async fn zero_runs_is_not_exact() {
        let s = measure(&MemoryBlob::new(), &[Leaf::Read], 0).await;
        assert!(!s[0].is_exact());
        assert_eq!(s[0].precision(), 0.0);
    }
}
