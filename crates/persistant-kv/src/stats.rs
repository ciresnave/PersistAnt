// SPDX-License-Identifier: MIT OR Apache-2.0
//! Stage 2 of backend benchmarking: **precision and per-operation statistics**. No timing (that is
//! stage 3) and no ranking.
//!
//! [`measure`] runs each leaf's test `runs` times, one attempt each (no retry: [`certify`] retries
//! the real-time leaves so a stalled machine cannot turn a correct backend off; a precision figure
//! must not hide the stall), and reports how often it passed, how often the backend broke the
//! contract and how often it errored, with the first failure's text. [`Instrumented`] counts, per
//! operation, the calls and the errors by kind that the backend produced while a test ran on it.
//!
//! A leaf that passed `runs` of `runs` is **exact**; anything less is a fraction with a count, never
//! rounded to a pass. The contract (stage 1) still decides eligibility: a backend with a failing run
//! is excluded however high its fraction.
//!
//! [`certify`]: crate::conformance::certify

use crate::conformance::{Failure, Leaf, run_leaf_once};
use crate::{BatchOp, KeyPage, KvBackend, KvError, ValuePage};
use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::Duration;

/// What one operation did while a test ran: calls, successes and errors by kind.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OpCount {
    /// Times it was called.
    pub calls: u64,
    /// Times it returned `Ok`.
    pub ok: u64,
    /// `KvError::Backend` results.
    pub backend: u64,
    /// `KvError::Unsupported` results.
    pub unsupported: u64,
    /// `KvError::Corrupt` results.
    pub corrupt: u64,
}

impl OpCount {
    /// Every error, whatever its kind.
    pub fn errors(&self) -> u64 {
        self.backend + self.unsupported + self.corrupt
    }
}

/// A [`KvBackend`] that counts what the wrapped backend does, per operation. It changes nothing.
#[derive(Debug)]
pub struct Instrumented<'a, K> {
    inner: &'a K,
    counts: Mutex<BTreeMap<&'static str, OpCount>>,
}

impl<'a, K: KvBackend> Instrumented<'a, K> {
    /// Wrap `inner`.
    pub fn new(inner: &'a K) -> Self {
        Instrumented {
            inner,
            counts: Mutex::new(BTreeMap::new()),
        }
    }

    /// The counts so far, by operation name (`get`, `put`, `batch`, ...).
    pub fn counts(&self) -> BTreeMap<&'static str, OpCount> {
        self.counts.lock().map(|c| c.clone()).unwrap_or_default()
    }

    fn note<T>(&self, op: &'static str, r: Result<T, KvError>) -> Result<T, KvError> {
        if let Ok(mut c) = self.counts.lock() {
            let e = c.entry(op).or_default();
            e.calls += 1;
            match &r {
                Ok(_) => e.ok += 1,
                Err(KvError::Backend(_)) => e.backend += 1,
                Err(KvError::Unsupported(_)) => e.unsupported += 1,
                Err(KvError::Corrupt(_)) => e.corrupt += 1,
            }
        }
        r
    }
}

impl<K: KvBackend> KvBackend for Instrumented<'_, K> {
    fn backend(&self) -> &str {
        self.inner.backend()
    }

    fn version(&self) -> &str {
        self.inner.version()
    }

    async fn get(&self, key: &str) -> Result<Option<Vec<u8>>, KvError> {
        self.note("get", self.inner.get(key).await)
    }

    async fn put(&self, key: &str, value: Vec<u8>, ttl: Option<Duration>) -> Result<(), KvError> {
        self.note("put", self.inner.put(key, value, ttl).await)
    }

    async fn delete(&self, key: &str) -> Result<(), KvError> {
        self.note("delete", self.inner.delete(key).await)
    }

    async fn put_if_absent(
        &self,
        key: &str,
        value: Vec<u8>,
        ttl: Option<Duration>,
    ) -> Result<bool, KvError> {
        self.note(
            "put_if_absent",
            self.inner.put_if_absent(key, value, ttl).await,
        )
    }

    async fn prefix_list(
        &self,
        prefix: &str,
        after: Option<&str>,
        limit: usize,
    ) -> Result<KeyPage, KvError> {
        self.note(
            "prefix_list",
            self.inner.prefix_list(prefix, after, limit).await,
        )
    }

    async fn ordered_scan(
        &self,
        prefix: &str,
        after: Option<&str>,
        limit: usize,
    ) -> Result<KeyPage, KvError> {
        self.note(
            "ordered_scan",
            self.inner.ordered_scan(prefix, after, limit).await,
        )
    }

    async fn compare_and_swap(
        &self,
        key: &str,
        expected: &[u8],
        new: Vec<u8>,
        ttl: Option<Duration>,
    ) -> Result<bool, KvError> {
        self.note(
            "compare_and_swap",
            self.inner.compare_and_swap(key, expected, new, ttl).await,
        )
    }

    async fn compare_and_delete(&self, key: &str, expected: &[u8]) -> Result<bool, KvError> {
        self.note(
            "compare_and_delete",
            self.inner.compare_and_delete(key, expected).await,
        )
    }

    async fn ttl_remaining(&self, key: &str) -> Result<Option<Duration>, KvError> {
        self.note("ttl_remaining", self.inner.ttl_remaining(key).await)
    }

    async fn purge_expired(&self) -> Result<u64, KvError> {
        self.note("purge_expired", self.inner.purge_expired().await)
    }

    async fn batch(&self, ops: &[BatchOp]) -> Result<bool, KvError> {
        self.note("batch", self.inner.batch(ops).await)
    }

    async fn ordered_scan_values(
        &self,
        prefix: &str,
        after: Option<&str>,
        limit: usize,
    ) -> Result<ValuePage, KvError> {
        self.note(
            "ordered_scan_values",
            self.inner.ordered_scan_values(prefix, after, limit).await,
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

    /// Passed runs over runs, `0.0` for no runs. A figure for reading; use
    /// [`is_exact`](Self::is_exact) to decide.
    pub fn precision(&self) -> f64 {
        if self.runs == 0 {
            0.0
        } else {
            f64::from(self.passed) / f64::from(self.runs)
        }
    }
}

/// Run each leaf's test `runs` times on `target` (one attempt per run) and count.
pub async fn measure<K: KvBackend>(target: &K, leaves: &[Leaf], runs: u32) -> Vec<LeafStats> {
    let mut out = Vec::with_capacity(leaves.len());
    for leaf in leaves.iter().copied() {
        let counted = Instrumented::new(target);
        let (mut passed, mut wrong, mut errored) = (0, 0, 0);
        let mut first_failure = None;
        for _ in 0..runs {
            match run_leaf_once(leaf, &counted).await {
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
    use crate::{Defect, KvOnBlob, MemoryKv};
    use persistant_blob::MemoryBlob;

    #[tokio::test]
    async fn a_correct_backend_is_exact_on_every_run_and_the_counts_add_up() {
        let s = measure(&MemoryKv::new(), &[Leaf::Get, Leaf::PutIfAbsent], 3).await;
        for l in &s {
            assert!(l.is_exact(), "{:?}", l);
            assert_eq!(l.precision(), 1.0);
            assert_eq!((l.wrong, l.errored), (0, 0));
        }
        let pia = &s[1].ops["put_if_absent"];
        assert_eq!(pia.calls, pia.ok + pia.errors());
        assert!(pia.calls >= 3, "the test calls it on every run");
    }

    #[tokio::test]
    async fn a_defect_is_counted_as_wrong_not_rounded_to_a_pass() {
        let kv = MemoryKv::with_defect(Defect::DeleteIsIgnored);
        let s = measure(&kv, &[Leaf::Delete], 4).await;
        assert_eq!((s[0].passed, s[0].wrong, s[0].errored), (0, 4, 0));
        assert!(!s[0].is_exact());
        assert!(s[0].first_failure.is_some());
    }

    #[tokio::test]
    async fn an_unsupported_operation_is_counted_as_an_error_by_kind() {
        let kv = KvOnBlob::new(MemoryBlob::new());
        let s = measure(&kv, &[Leaf::BatchRollback], 2).await;
        assert_eq!((s[0].passed, s[0].errored), (0, 2));
        let batch = &s[0].ops["batch"];
        assert!(batch.unsupported >= 2, "{batch:?}");
        assert_eq!((batch.backend, batch.corrupt), (0, 0));
    }

    #[tokio::test]
    async fn zero_runs_is_not_exact() {
        let s = measure(&MemoryKv::new(), &[Leaf::Get], 0).await;
        assert!(!s[0].is_exact());
        assert_eq!(s[0].precision(), 0.0);
    }
}
