// SPDX-License-Identifier: MIT OR Apache-2.0
use crate::{BlobBackend, BlobError, Entry, Page};
use std::collections::BTreeMap;
use std::sync::Mutex;

/// A deliberate fault, for the known-bad control of a contract test.
///
/// A test that cannot fail proves nothing; each variant breaks exactly one leaf's contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Defect {
    /// `read` drops the last byte.
    ReadTruncates,
    /// `write` is acknowledged and not stored.
    WriteIsLost,
    /// `delete` is acknowledged and the key stays.
    DeleteIsIgnored,
    /// `stat` reports a length one too long.
    StatLiesAboutLength,
    /// `list` returns keys in descending order.
    ListIsDescending,
    /// `list` treats `%` and `[` in the prefix as pattern characters, not literals.
    ListPrefixIsPattern,
    /// `list` ignores `limit` and `after`.
    ListIgnoresPaging,
    /// `create_exclusive` checks, lets other callers run, then writes: two callers can both win.
    CreateNotExclusive,
    /// `replace_atomic` writes the first half, lets readers run, then the rest: a reader can see a
    /// mix of old and new.
    ReplaceTearsReads,
}

/// An in-memory reference backend. Correct unless built with [`MemoryBlob::with_defect`].
#[derive(Debug, Default)]
pub struct MemoryBlob {
    map: Mutex<BTreeMap<String, Vec<u8>>>,
    defect: Option<Defect>,
}

impl MemoryBlob {
    /// A correct, empty store.
    pub fn new() -> Self {
        MemoryBlob::default()
    }

    /// A store that is wrong in exactly one way.
    pub fn with_defect(defect: Defect) -> Self {
        MemoryBlob {
            map: Mutex::default(),
            defect: Some(defect),
        }
    }

    fn map(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, Vec<u8>>> {
        // A poisoned lock means a test panicked mid-operation; the data is still usable.
        self.map.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// A prefix match that wrongly reads `%` and `[` as pattern characters (what a LIKE or glob
/// based backend does): the prefix is cut at the first one.
fn pattern_prefix_match(key: &str, pattern: &str) -> bool {
    let cut = pattern.find(['%', '[']).unwrap_or(pattern.len());
    key.starts_with(&pattern[..cut])
}

/// Let other tasks polled by the same executor run once. Used by defects and by the harness.
pub(crate) async fn yield_now() {
    let mut yielded = false;
    std::future::poll_fn(|cx| {
        if yielded {
            std::task::Poll::Ready(())
        } else {
            yielded = true;
            cx.waker().wake_by_ref();
            std::task::Poll::Pending
        }
    })
    .await;
}

impl BlobBackend for MemoryBlob {
    fn backend(&self) -> &str {
        "memory"
    }

    fn version(&self) -> &str {
        env!("CARGO_PKG_VERSION")
    }

    async fn read(&self, key: &str) -> Result<Option<Vec<u8>>, BlobError> {
        let mut v = self.map().get(key).cloned();
        if self.defect == Some(Defect::ReadTruncates)
            && let Some(b) = v.as_mut()
        {
            b.pop();
        }
        Ok(v)
    }

    async fn write(&self, key: &str, bytes: Vec<u8>) -> Result<(), BlobError> {
        if self.defect != Some(Defect::WriteIsLost) {
            self.map().insert(key.to_string(), bytes);
        }
        Ok(())
    }

    async fn delete(&self, key: &str) -> Result<(), BlobError> {
        if self.defect != Some(Defect::DeleteIsIgnored) {
            self.map().remove(key);
        }
        Ok(())
    }

    async fn stat(&self, key: &str) -> Result<Option<Entry>, BlobError> {
        let extra = u64::from(self.defect == Some(Defect::StatLiesAboutLength));
        Ok(self
            .map()
            .get(key)
            .map(|v| Entry::new(key, v.len() as u64 + extra)))
    }

    async fn list(
        &self,
        prefix: &str,
        after: Option<&str>,
        limit: usize,
    ) -> Result<Page, BlobError> {
        let map = self.map();
        let glob = self.defect == Some(Defect::ListPrefixIsPattern);
        let ignore_paging = self.defect == Some(Defect::ListIgnoresPaging);
        let mut keys: Vec<&String> = map
            .keys()
            .filter(|k| {
                if glob {
                    pattern_prefix_match(k, prefix)
                } else {
                    k.starts_with(prefix)
                }
            })
            .filter(|k| ignore_paging || after.is_none_or(|a| k.as_str() > a))
            .collect();
        if self.defect == Some(Defect::ListIsDescending) {
            keys.reverse();
        }
        let limit = if ignore_paging { usize::MAX } else { limit };
        let more = keys.len() > limit;
        let entries: Vec<Entry> = keys
            .into_iter()
            .take(limit)
            .map(|k| Entry::new(k.clone(), map[k].len() as u64))
            .collect();
        let next = if more {
            entries.last().map(|e| e.key.clone())
        } else {
            None
        };
        Ok(Page::new(entries, next))
    }

    async fn create_exclusive(&self, key: &str, bytes: Vec<u8>) -> Result<bool, BlobError> {
        if self.defect == Some(Defect::CreateNotExclusive) {
            let absent = !self.map().contains_key(key);
            yield_now().await;
            if absent {
                self.map().insert(key.to_string(), bytes);
            }
            return Ok(absent);
        }
        let mut map = self.map();
        if map.contains_key(key) {
            return Ok(false);
        }
        map.insert(key.to_string(), bytes);
        Ok(true)
    }

    async fn replace_atomic(&self, key: &str, bytes: Vec<u8>) -> Result<(), BlobError> {
        if self.defect == Some(Defect::ReplaceTearsReads) {
            let half = bytes.len() / 2;
            let mut torn = bytes[..half].to_vec();
            if let Some(old) = self.map().get(key) {
                torn.extend_from_slice(old.get(half..).unwrap_or(&[]));
            } else {
                torn.extend_from_slice(&bytes[half..]);
            }
            self.map().insert(key.to_string(), torn);
            yield_now().await;
        }
        self.map().insert(key.to_string(), bytes);
        Ok(())
    }
}
