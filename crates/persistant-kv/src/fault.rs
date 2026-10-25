// SPDX-License-Identifier: MIT OR Apache-2.0
//! Fault injection for the key-value kind (`docs/DESIGN-KINDS.md` section 5, audit row 7): a
//! [`Faulty`] wrapper that makes chosen calls of any [`KvBackend`] fail, so a consumer can test what
//! it does when its store misbehaves, without a real outage.
//!
//! - [`Faulty::fail_nth`]: the Nth call fails **before** it runs; nothing happened.
//! - [`Faulty::interrupt_nth`]: the Nth call **runs, then reports an error**: the outcome is
//!   ambiguous to the caller, as after a lost reply or a timeout of a write.
//! - [`Faulty::mask`]: an operation answers [`KvError::Unsupported`], as a backend without that
//!   capability would.
//!
//! "The Nth" counts calls of one operation (`Some("put")`) or of every operation (`None`), from 1.
//! A rule fires once. The wrapper changes nothing else.

use crate::{BatchOp, BoundedProbe, KeyPage, KvBackend, KvError, ValuePage};
use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::sync::Mutex;
use std::time::Duration;

#[derive(Debug)]
struct Rule {
    op: Option<&'static str>,
    nth: u64,
    error: KvError,
    /// Run the call first and drop its result.
    after_effect: bool,
}

#[derive(Debug, Default)]
struct State {
    total: u64,
    per_op: BTreeMap<&'static str, u64>,
    rules: Vec<Rule>,
    masked: BTreeSet<&'static str>,
}

/// A [`KvBackend`] that fails, interrupts or masks chosen calls of the wrapped backend.
#[derive(Debug)]
pub struct Faulty<K> {
    inner: K,
    state: Mutex<State>,
}

impl<K: KvBackend> Faulty<K> {
    /// Wrap `inner`; with no rules it behaves as `inner` does.
    pub fn new(inner: K) -> Self {
        Faulty {
            inner,
            state: Mutex::new(State::default()),
        }
    }

    /// The wrapped backend.
    pub fn into_inner(self) -> K {
        self.inner
    }

    /// The Nth call of `op` (or of any operation, for `None`) returns `error` without running.
    #[must_use]
    pub fn fail_nth(self, op: Option<&'static str>, nth: u64, error: KvError) -> Self {
        self.rule(op, nth, error, false)
    }

    /// The Nth call of `op` (or of any operation, for `None`) runs, then returns `error`: whatever
    /// it did stays done.
    #[must_use]
    pub fn interrupt_nth(self, op: Option<&'static str>, nth: u64, error: KvError) -> Self {
        self.rule(op, nth, error, true)
    }

    /// Every call of `op` returns [`KvError::Unsupported`] without running.
    #[must_use]
    pub fn mask(self, op: &'static str) -> Self {
        if let Ok(mut s) = self.state.lock() {
            s.masked.insert(op);
        }
        self
    }

    /// How many calls of `op` have been made, counting those that were failed or masked.
    pub fn calls(&self, op: &'static str) -> u64 {
        self.state
            .lock()
            .map(|s| s.per_op.get(op).copied().unwrap_or(0))
            .unwrap_or(0)
    }

    fn rule(self, op: Option<&'static str>, nth: u64, error: KvError, after_effect: bool) -> Self {
        if let Ok(mut s) = self.state.lock() {
            s.rules.push(Rule {
                op,
                nth,
                error,
                after_effect,
            });
        }
        self
    }

    /// Count the call and decide its fate: `Ok(None)` run it, `Ok(Some(rule))` a fired rule,
    /// `Err` masked.
    fn decide(&self, op: &'static str) -> Result<Option<Rule>, KvError> {
        let Ok(mut s) = self.state.lock() else {
            return Ok(None);
        };
        s.total += 1;
        let total = s.total;
        let n = {
            let c = s.per_op.entry(op).or_default();
            *c += 1;
            *c
        };
        if s.masked.contains(op) {
            return Err(KvError::Unsupported(op.to_string()));
        }
        let hit = s.rules.iter().position(|r| match r.op {
            Some(o) => o == op && r.nth == n,
            None => r.nth == total,
        });
        Ok(hit.map(|i| s.rules.remove(i)))
    }

    async fn run<T>(
        &self,
        op: &'static str,
        call: impl Future<Output = Result<T, KvError>>,
    ) -> Result<T, KvError> {
        match self.decide(op)? {
            None => call.await,
            Some(rule) => {
                if rule.after_effect {
                    let _ = call.await;
                }
                Err(rule.error)
            }
        }
    }
}

impl<K: KvBackend> KvBackend for Faulty<K> {
    fn backend(&self) -> &str {
        self.inner.backend()
    }

    fn version(&self) -> &str {
        self.inner.version()
    }

    fn call_bound(&self) -> Option<Duration> {
        self.inner.call_bound()
    }

    fn bounded_call_probe(&self) -> impl Future<Output = Result<BoundedProbe, KvError>> + Send {
        self.inner.bounded_call_probe()
    }

    async fn get(&self, key: &str) -> Result<Option<Vec<u8>>, KvError> {
        self.run("get", self.inner.get(key)).await
    }

    async fn put(&self, key: &str, value: Vec<u8>, ttl: Option<Duration>) -> Result<(), KvError> {
        self.run("put", self.inner.put(key, value, ttl)).await
    }

    async fn delete(&self, key: &str) -> Result<(), KvError> {
        self.run("delete", self.inner.delete(key)).await
    }

    async fn put_if_absent(
        &self,
        key: &str,
        value: Vec<u8>,
        ttl: Option<Duration>,
    ) -> Result<bool, KvError> {
        self.run("put_if_absent", self.inner.put_if_absent(key, value, ttl))
            .await
    }

    async fn prefix_list(
        &self,
        prefix: &str,
        after: Option<&str>,
        limit: usize,
    ) -> Result<KeyPage, KvError> {
        self.run("prefix_list", self.inner.prefix_list(prefix, after, limit))
            .await
    }

    async fn ordered_scan(
        &self,
        prefix: &str,
        after: Option<&str>,
        limit: usize,
    ) -> Result<KeyPage, KvError> {
        self.run(
            "ordered_scan",
            self.inner.ordered_scan(prefix, after, limit),
        )
        .await
    }

    async fn compare_and_swap(
        &self,
        key: &str,
        expected: &[u8],
        new: Vec<u8>,
        ttl: Option<Duration>,
    ) -> Result<bool, KvError> {
        self.run(
            "compare_and_swap",
            self.inner.compare_and_swap(key, expected, new, ttl),
        )
        .await
    }

    async fn compare_and_delete(&self, key: &str, expected: &[u8]) -> Result<bool, KvError> {
        self.run(
            "compare_and_delete",
            self.inner.compare_and_delete(key, expected),
        )
        .await
    }

    async fn take(&self, key: &str) -> Result<Option<Vec<u8>>, KvError> {
        self.run("take", self.inner.take(key)).await
    }

    async fn ttl_remaining(&self, key: &str) -> Result<Option<Duration>, KvError> {
        self.run("ttl_remaining", self.inner.ttl_remaining(key))
            .await
    }

    async fn purge_expired(&self) -> Result<u64, KvError> {
        self.run("purge_expired", self.inner.purge_expired()).await
    }

    async fn count(&self, prefix: &str) -> Result<u64, KvError> {
        self.run("count", self.inner.count(prefix)).await
    }

    async fn batch(&self, ops: &[BatchOp]) -> Result<bool, KvError> {
        self.run("batch", self.inner.batch(ops)).await
    }

    async fn ordered_scan_values(
        &self,
        prefix: &str,
        after: Option<&str>,
        limit: usize,
    ) -> Result<ValuePage, KvError> {
        self.run(
            "ordered_scan_values",
            self.inner.ordered_scan_values(prefix, after, limit),
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MemoryKv;

    fn boom() -> KvError {
        KvError::Backend("boom".to_string())
    }

    #[tokio::test]
    async fn with_no_rules_it_is_the_wrapped_backend() {
        let kv = Faulty::new(MemoryKv::new());
        kv.put("a", b"1".to_vec(), None).await.unwrap();
        assert_eq!(kv.get("a").await.unwrap().unwrap(), b"1");
        assert_eq!(kv.calls("put"), 1);
        assert_eq!(kv.calls("get"), 1);
    }

    #[tokio::test]
    async fn the_nth_call_of_one_operation_fails_before_it_runs_and_only_once() {
        let kv = Faulty::new(MemoryKv::new()).fail_nth(Some("put"), 2, boom());
        kv.put("a", b"1".to_vec(), None).await.unwrap();
        assert_eq!(kv.get("a").await.unwrap().unwrap(), b"1");
        assert_eq!(kv.put("a", b"2".to_vec(), None).await, Err(boom()));
        // Nothing happened: the first value is still there.
        assert_eq!(kv.get("a").await.unwrap().unwrap(), b"1");
        // The rule fired once; the next put works.
        kv.put("a", b"3".to_vec(), None).await.unwrap();
        assert_eq!(kv.get("a").await.unwrap().unwrap(), b"3");
        assert_eq!(kv.calls("put"), 3);
    }

    #[tokio::test]
    async fn an_interrupted_call_runs_and_then_reports_the_error() {
        let kv = Faulty::new(MemoryKv::new()).interrupt_nth(Some("put"), 1, boom());
        assert_eq!(kv.put("a", b"1".to_vec(), None).await, Err(boom()));
        // The caller was told it failed; the write happened anyway.
        assert_eq!(kv.get("a").await.unwrap().unwrap(), b"1");
    }

    #[tokio::test]
    async fn none_counts_every_operation() {
        let kv = Faulty::new(MemoryKv::new()).fail_nth(None, 3, boom());
        kv.put("a", b"1".to_vec(), None).await.unwrap();
        kv.get("a").await.unwrap();
        assert_eq!(kv.delete("a").await, Err(boom()));
        assert_eq!(kv.get("a").await.unwrap().unwrap(), b"1");
    }

    #[tokio::test]
    async fn a_masked_operation_is_unsupported_and_the_others_work() {
        let kv = Faulty::new(MemoryKv::new()).mask("count");
        kv.put("a", b"1".to_vec(), None).await.unwrap();
        assert_eq!(
            kv.count("").await,
            Err(KvError::Unsupported("count".to_string()))
        );
        assert_eq!(kv.get("a").await.unwrap().unwrap(), b"1");
        // Control: the unwrapped backend does count.
        let plain = MemoryKv::new();
        plain.put("a", b"1".to_vec(), None).await.unwrap();
        assert_eq!(plain.count("").await, Ok(1));
    }
}
