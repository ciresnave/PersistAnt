// SPDX-License-Identifier: MIT OR Apache-2.0
use crate::{BatchOp, KeyPage, KvBackend, KvError, ValuePage};
use persistant_blob::concurrency::yield_now;
use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// A deliberate fault, for the known-bad control of a contract test.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Defect {
    /// `put` is acknowledged and not stored.
    PutIsLost,
    /// Keys that differ only in letter case are the same key.
    KeysFoldCase,
    /// `delete` is acknowledged and the key stays.
    DeleteIsIgnored,
    /// A time to live is dropped: the entry never expires.
    TtlIgnored,
    /// `get` honours the time to live and listing still shows expired keys.
    ListShowsExpired,
    /// `put_if_absent` checks, lets others run, then writes: two callers can both win.
    PutIfAbsentNotExclusive,
    /// `prefix_list` treats `%` and `[` in the prefix as pattern characters.
    ListPrefixIsPattern,
    /// `ordered_scan` returns keys in descending order.
    ScanIsDescending,
    /// `ordered_scan` orders case-insensitively, as a text column under a case-insensitive or
    /// locale collation does (the trap `BYTEA` keys avoid).
    ScanIsCaseInsensitive,
    /// Paged calls ignore `limit` and `after`: the same keys come back every page.
    ListIgnoresPaging,
    /// `batch` applies its operations one at a time with other callers running in between, and
    /// does not undo the earlier ones when a precondition later fails.
    BatchNotAtomic,
    /// `batch` ignores its `Require` operations.
    BatchIgnoresRequire,
    /// `ttl_remaining` always reports an hour, whatever was written.
    TtlRemainingIsConstant,
    /// `purge_expired` removes every entry, live ones too.
    PurgeRemovesLive,
    /// `compare_and_swap` swaps whatever is there, ignoring `expected`.
    CasIgnoresExpected,
    /// `compare_and_swap` checks, lets others run, then writes: two callers can both win.
    CasNotAtomic,
    /// `compare_and_delete` deletes whatever is there, ignoring `expected`.
    CadIgnoresExpected,
    /// `compare_and_delete` checks, lets others run, then deletes: two callers can both win.
    CadNotAtomic,
    /// A call blocked past its bound is never cut off: it finishes late, or not at all.
    BoundIgnored,
    /// A batch cut off by the timeout is applied anyway once the block is gone.
    TimeoutLeavesBatchApplied,
}

type Entry = (Vec<u8>, Option<Instant>);

/// An in-memory reference backend. Correct unless built with [`MemoryKv::with_defect`].
#[derive(Debug, Default)]
pub struct MemoryKv {
    map: Mutex<BTreeMap<String, Entry>>,
    defect: Option<Defect>,
    bound: Option<Duration>,
}

fn live(e: &Entry) -> bool {
    e.1.is_none_or(|t| Instant::now() < t)
}

impl MemoryKv {
    /// A correct, empty store.
    pub fn new() -> Self {
        MemoryKv::default()
    }

    /// A store that is wrong in exactly one way.
    pub fn with_defect(defect: Defect) -> Self {
        MemoryKv {
            map: Mutex::default(),
            defect: Some(defect),
            bound: None,
        }
    }

    /// A correct store that reports a per-call bound, as the reference for `kv.bounded_call`. It
    /// never blocks, so its probe is simulated: it exercises the leaf's judgement of a probe report
    /// (the controls), not a real timeout. The real thing is `KvOnRelational` over a bounded
    /// backend.
    pub fn bounded() -> Self {
        MemoryKv {
            bound: Some(Duration::from_millis(50)),
            ..MemoryKv::default()
        }
    }

    fn k(&self, key: &str) -> String {
        if self.defect == Some(Defect::KeysFoldCase) {
            key.to_lowercase()
        } else {
            key.to_string()
        }
    }

    fn map(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, Entry>> {
        self.map.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn listing(&self, prefix: &str, after: Option<&str>, limit: usize, ordered: bool) -> KeyPage {
        let map = self.map();
        let pattern = self.defect == Some(Defect::ListPrefixIsPattern);
        let show_expired = self.defect == Some(Defect::ListShowsExpired);
        let ignore_paging = self.defect == Some(Defect::ListIgnoresPaging);
        let cut = if pattern {
            prefix.find(['%', '[']).unwrap_or(prefix.len())
        } else {
            prefix.len()
        };
        let mut keys: Vec<String> = map
            .iter()
            .filter(|(k, e)| k.starts_with(&prefix[..cut]) && (show_expired || live(e)))
            .filter(|(k, _)| ignore_paging || after.is_none_or(|a| k.as_str() > a))
            .map(|(k, _)| k.clone())
            .collect();
        if ordered && self.defect == Some(Defect::ScanIsDescending) {
            keys.reverse();
        }
        if ordered && self.defect == Some(Defect::ScanIsCaseInsensitive) {
            keys.sort_by_key(|k| k.to_lowercase());
        }
        let limit = if ignore_paging { usize::MAX } else { limit };
        let more = keys.len() > limit;
        keys.truncate(limit);
        let next = if more { keys.last().cloned() } else { None };
        KeyPage::new(keys, next)
    }
}

impl KvBackend for MemoryKv {
    fn backend(&self) -> &str {
        "memory"
    }

    fn version(&self) -> &str {
        env!("CARGO_PKG_VERSION")
    }

    fn call_bound(&self) -> Option<Duration> {
        match self.defect {
            Some(Defect::BoundIgnored | Defect::TimeoutLeavesBatchApplied) => {
                Some(Duration::from_millis(50))
            }
            _ => self.bound,
        }
    }

    async fn bounded_call_probe(&self) -> Result<crate::BoundedProbe, KvError> {
        let bound = self
            .call_bound()
            .ok_or_else(|| KvError::Unsupported("no call timeout configured".to_string()))?;
        let mut p = crate::BoundedProbe {
            bound,
            elapsed: bound,
            finished: true,
            error: Some(KvError::TimedOut(bound)),
            applied_after: false,
            recovered: true,
        };
        match self.defect {
            Some(Defect::BoundIgnored) => {
                p.elapsed = bound * 100;
                p.finished = false;
                p.error = None;
            }
            Some(Defect::TimeoutLeavesBatchApplied) => p.applied_after = true,
            _ => {}
        }
        Ok(p)
    }

    async fn get(&self, key: &str) -> Result<Option<Vec<u8>>, KvError> {
        Ok(self
            .map()
            .get(&self.k(key))
            .filter(|e| live(e))
            .map(|e| e.0.clone()))
    }

    async fn put(&self, key: &str, value: Vec<u8>, ttl: Option<Duration>) -> Result<(), KvError> {
        if self.defect == Some(Defect::PutIsLost) {
            return Ok(());
        }
        let ttl = if self.defect == Some(Defect::TtlIgnored) {
            None
        } else {
            ttl
        };
        self.map()
            .insert(self.k(key), (value, ttl.map(|d| Instant::now() + d)));
        Ok(())
    }

    async fn delete(&self, key: &str) -> Result<(), KvError> {
        if self.defect != Some(Defect::DeleteIsIgnored) {
            self.map().remove(key);
        }
        Ok(())
    }

    async fn put_if_absent(
        &self,
        key: &str,
        value: Vec<u8>,
        ttl: Option<Duration>,
    ) -> Result<bool, KvError> {
        let entry = (value, ttl.map(|d| Instant::now() + d));
        if self.defect == Some(Defect::PutIfAbsentNotExclusive) {
            let absent = !self.map().get(key).is_some_and(live);
            yield_now().await;
            if absent {
                self.map().insert(key.to_string(), entry);
            }
            return Ok(absent);
        }
        let mut map = self.map();
        if map.get(key).is_some_and(live) {
            return Ok(false);
        }
        map.insert(key.to_string(), entry);
        Ok(true)
    }

    async fn prefix_list(
        &self,
        prefix: &str,
        after: Option<&str>,
        limit: usize,
    ) -> Result<KeyPage, KvError> {
        Ok(self.listing(prefix, after, limit, false))
    }

    async fn ordered_scan(
        &self,
        prefix: &str,
        after: Option<&str>,
        limit: usize,
    ) -> Result<KeyPage, KvError> {
        Ok(self.listing(prefix, after, limit, true))
    }

    async fn compare_and_swap(
        &self,
        key: &str,
        expected: &[u8],
        new: Vec<u8>,
        ttl: Option<Duration>,
    ) -> Result<bool, KvError> {
        let entry = (new, ttl.map(|d| Instant::now() + d));
        let matches = |map: &BTreeMap<String, Entry>| {
            map.get(key).is_some_and(|e| live(e) && e.0 == expected)
        };
        if self.defect == Some(Defect::CasIgnoresExpected) {
            self.map().insert(key.to_string(), entry);
            return Ok(true);
        }
        if self.defect == Some(Defect::CasNotAtomic) {
            let ok = matches(&self.map());
            yield_now().await;
            if ok {
                self.map().insert(key.to_string(), entry);
            }
            return Ok(ok);
        }
        let mut map = self.map();
        if matches(&map) {
            map.insert(key.to_string(), entry);
            Ok(true)
        } else {
            Ok(false)
        }
    }

    async fn compare_and_delete(&self, key: &str, expected: &[u8]) -> Result<bool, KvError> {
        let matches = |map: &BTreeMap<String, Entry>| {
            map.get(key).is_some_and(|e| live(e) && e.0 == expected)
        };
        if self.defect == Some(Defect::CadIgnoresExpected) {
            return Ok(self.map().remove(key).is_some());
        }
        if self.defect == Some(Defect::CadNotAtomic) {
            let ok = matches(&self.map());
            yield_now().await;
            if ok {
                self.map().remove(key);
            }
            return Ok(ok);
        }
        let mut map = self.map();
        if matches(&map) {
            map.remove(key);
            Ok(true)
        } else {
            Ok(false)
        }
    }

    async fn ttl_remaining(&self, key: &str) -> Result<Option<Duration>, KvError> {
        let map = self.map();
        let Some(e) = map.get(&self.k(key)).filter(|e| live(e)) else {
            return Ok(None);
        };
        if self.defect == Some(Defect::TtlRemainingIsConstant) {
            return Ok(Some(Duration::from_secs(3600)));
        }
        Ok(e.1.map(|t| t.saturating_duration_since(Instant::now())))
    }

    async fn purge_expired(&self) -> Result<u64, KvError> {
        let mut map = self.map();
        let before = map.len();
        if self.defect == Some(Defect::PurgeRemovesLive) {
            map.clear();
        } else {
            map.retain(|_, e| live(e));
        }
        Ok((before - map.len()) as u64)
    }

    async fn batch(&self, ops: &[BatchOp]) -> Result<bool, KvError> {
        let live_value = |map: &BTreeMap<String, Entry>, key: &str| {
            map.get(key).filter(|e| live(e)).map(|e| e.0.clone())
        };
        if self.defect == Some(Defect::BatchNotAtomic) {
            // One operation at a time, yielding in between, and no undo.
            for op in ops {
                match op {
                    BatchOp::Require { key, expected } => {
                        let now = live_value(&self.map(), key);
                        if now != *expected {
                            return Ok(false);
                        }
                    }
                    BatchOp::Put { key, value, ttl } => {
                        self.map().insert(
                            self.k(key),
                            (value.clone(), ttl.map(|d| Instant::now() + d)),
                        );
                    }
                    BatchOp::Delete { key } => {
                        self.map().remove(&self.k(key));
                    }
                }
                yield_now().await;
            }
            return Ok(true);
        }
        let mut map = self.map();
        if self.defect != Some(Defect::BatchIgnoresRequire) {
            for op in ops {
                if let BatchOp::Require { key, expected } = op
                    && live_value(&map, key) != *expected
                {
                    return Ok(false);
                }
            }
        }
        for op in ops {
            match op {
                BatchOp::Put { key, value, ttl } => {
                    let k = self.k(key);
                    map.insert(k, (value.clone(), ttl.map(|d| Instant::now() + d)));
                }
                BatchOp::Delete { key } => {
                    map.remove(&self.k(key));
                }
                BatchOp::Require { .. } => {}
            }
        }
        Ok(true)
    }

    async fn ordered_scan_values(
        &self,
        prefix: &str,
        after: Option<&str>,
        limit: usize,
    ) -> Result<ValuePage, KvError> {
        let map = self.map();
        let ignore_paging = self.defect == Some(Defect::ListIgnoresPaging);
        let mut keys: Vec<(&String, &Entry)> = map
            .iter()
            .filter(|(k, e)| k.starts_with(prefix) && live(e))
            .filter(|(k, _)| ignore_paging || after.is_none_or(|a| k.as_str() > a))
            .collect();
        if self.defect == Some(Defect::ScanIsDescending) {
            keys.reverse();
        }
        let limit = if ignore_paging { usize::MAX } else { limit };
        let more = keys.len() > limit;
        keys.truncate(limit);
        let entries: Vec<(String, Vec<u8>)> = keys
            .into_iter()
            .map(|(k, e)| (k.clone(), e.0.clone()))
            .collect();
        let next = if more {
            entries.last().map(|(k, _)| k.clone())
        } else {
            None
        };
        Ok(ValuePage::new(entries, next))
    }
}
