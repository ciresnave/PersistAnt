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
    /// `list` treats the prefix as a glob where `*` matches anything.
    ListPrefixIsGlob,
    /// `list` ignores `limit` and `after`.
    ListIgnoresPaging,
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

fn glob_prefix_match(key: &str, pattern: &str) -> bool {
    match pattern.split_once('*') {
        None => key.starts_with(pattern),
        Some((head, _)) => key.starts_with(head),
    }
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
        let glob = self.defect == Some(Defect::ListPrefixIsGlob);
        let ignore_paging = self.defect == Some(Defect::ListIgnoresPaging);
        let mut keys: Vec<&String> = map
            .keys()
            .filter(|k| {
                if glob {
                    glob_prefix_match(k, prefix)
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
}
