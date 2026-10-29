// SPDX-License-Identifier: MIT OR Apache-2.0
use crate::concurrency::yield_now;
use crate::{BlobBackend, BlobError, BlobSink, BoxFuture, Entry, KeyLayout, Page};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

/// A deliberate fault, for the known-bad control of a contract test.
///
/// A test that cannot fail proves nothing; each variant breaks exactly one leaf's contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Defect {
    /// `read` drops the last byte.
    ReadTruncates,
    /// Keys that differ only in letter case are the same key (a case-insensitive filesystem).
    KeysFoldCase,
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
    /// `append` replaces the value instead of adding to it.
    AppendOverwrites,
    /// `append` adds its first half, lets others run, then the rest: concurrent records interleave.
    AppendTearsRecords,
    /// `replace_atomic` writes the first half, lets readers run, then the rest: a reader can see a
    /// mix of old and new.
    ReplaceTearsReads,
    /// `read_range` ignores the offset and reads from the start.
    RangeIgnoresOffset,
    /// `read_range` drops the last byte of the range.
    RangeShort,
    /// `read_range_if` never reports a change.
    RangeIfIgnoresTag,
    /// The version tag is the length alone, so a same-length replacement keeps the tag.
    TagIgnoresContent,
    /// A streamed write goes straight to the key as chunks arrive: visible before `finish`, and it
    /// destroys the old value.
    StreamWritesThrough,
    /// A streamed write keeps only its first chunk.
    StreamDropsTail,
    /// `abort` and dropping a sink leave the written bytes staged at `<key>.staging`, where a
    /// `list` sees them.
    AbortLeavesStaging,
    /// `write_stream_atomic`'s `finish` publishes half the value, lets readers run, then the rest:
    /// a reader can see a mix of old and new.
    StreamAtomicTears,
}

/// An in-memory reference backend. Correct unless built with [`MemoryBlob::with_defect`].
#[derive(Debug, Default)]
pub struct MemoryBlob {
    map: Arc<Mutex<BTreeMap<String, Vec<u8>>>>,
    defect: Option<Defect>,
    hex_layout: bool,
}

impl MemoryBlob {
    /// A correct, empty store.
    pub fn new() -> Self {
        MemoryBlob::default()
    }

    /// A store that is wrong in exactly one way.
    pub fn with_defect(defect: Defect) -> Self {
        MemoryBlob {
            map: Arc::default(),
            defect: Some(defect),
            hex_layout: false,
        }
    }

    /// As [`MemoryBlob::new`] but asking the layer above for [`KeyLayout::HexPath`] keys, as a
    /// filesystem backend does (to test that layout without a filesystem).
    pub fn with_hex_layout(mut self) -> Self {
        self.hex_layout = true;
        self
    }

    fn k(&self, key: &str) -> String {
        if self.defect == Some(Defect::KeysFoldCase) {
            key.to_lowercase()
        } else {
            key.to_string()
        }
    }

    fn sink(&self, key: &str, atomic: bool) -> Box<dyn BlobSink> {
        Box::new(MemorySink {
            map: Arc::clone(&self.map),
            key: self.k(key),
            buf: Vec::new(),
            atomic,
            defect: self.defect,
            chunks: 0,
            done: false,
        })
    }

    fn map(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, Vec<u8>>> {
        // A poisoned lock means a test panicked mid-operation; the data is still usable.
        self.map.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// A version token for `v`: its length and a hash of its bytes (FNV-1a), so a same-length
/// replacement with other bytes has another tag. The reference backend pays O(n) per `stat`.
fn tag_of(v: &[u8], defect: Option<Defect>) -> String {
    if defect == Some(Defect::TagIgnoresContent) {
        return format!("len-{}", v.len());
    }
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in v {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{:016x}-{}", h, v.len())
}

/// The slice `[offset, offset + len)` of `v`, clamped to what exists.
fn slice_of(v: &[u8], offset: u64, len: u64) -> Vec<u8> {
    let start = usize::try_from(offset).map_or(v.len(), |o| o.min(v.len()));
    let end = usize::try_from(len).map_or(v.len(), |l| start.saturating_add(l).min(v.len()));
    v[start..end].to_vec()
}

/// The streamed-write handle of [`MemoryBlob`]: the bytes are held in the sink and put in the map
/// only on `finish`, unless a [`Defect`] says otherwise.
struct MemorySink {
    map: Arc<Mutex<BTreeMap<String, Vec<u8>>>>,
    key: String,
    buf: Vec<u8>,
    atomic: bool,
    defect: Option<Defect>,
    chunks: usize,
    done: bool,
}

impl MemorySink {
    fn map(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, Vec<u8>>> {
        self.map.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn leave_staging(&self) {
        if self.defect == Some(Defect::AbortLeavesStaging) {
            let staged = format!("{}.staging", self.key);
            let buf = self.buf.clone();
            self.map().insert(staged, buf);
        }
    }
}

impl Drop for MemorySink {
    fn drop(&mut self) {
        if !self.done {
            self.leave_staging();
        }
    }
}

impl BlobSink for MemorySink {
    fn write<'a>(&'a mut self, chunk: &'a [u8]) -> BoxFuture<'a, Result<(), BlobError>> {
        Box::pin(async move {
            let first = self.chunks == 0;
            self.chunks += 1;
            match self.defect {
                Some(Defect::StreamWritesThrough) => {
                    let key = self.key.clone();
                    let mut map = self.map();
                    let v = map.entry(key).or_default();
                    if first {
                        v.clear();
                    }
                    v.extend_from_slice(chunk);
                }
                Some(Defect::StreamDropsTail) if !first => {}
                _ => self.buf.extend_from_slice(chunk),
            }
            Ok(())
        })
    }

    fn finish(mut self: Box<Self>) -> BoxFuture<'static, Result<(), BlobError>> {
        Box::pin(async move {
            self.done = true;
            if self.defect == Some(Defect::StreamWritesThrough) {
                return Ok(());
            }
            let key = self.key.clone();
            let buf = std::mem::take(&mut self.buf);
            if self.atomic && self.defect == Some(Defect::StreamAtomicTears) {
                let half = buf.len() / 2;
                let mut torn = buf[..half].to_vec();
                match self.map().get(&key) {
                    Some(old) => torn.extend_from_slice(old.get(half..).unwrap_or(&[])),
                    None => torn.extend_from_slice(&buf[half..]),
                }
                self.map().insert(key.clone(), torn);
                yield_now().await;
            }
            self.map().insert(key, buf);
            Ok(())
        })
    }

    fn abort(mut self: Box<Self>) -> BoxFuture<'static, Result<(), BlobError>> {
        Box::pin(async move {
            self.done = true;
            self.leave_staging();
            Ok(())
        })
    }
}

/// A prefix match that wrongly reads `%` and `[` as pattern characters (what a LIKE or glob
/// based backend does): the prefix is cut at the first one.
fn pattern_prefix_match(key: &str, pattern: &str) -> bool {
    let cut = pattern.find(['%', '[']).unwrap_or(pattern.len());
    key.starts_with(&pattern[..cut])
}

impl BlobBackend for MemoryBlob {
    fn key_layout(&self) -> KeyLayout {
        if self.hex_layout {
            KeyLayout::HexPath
        } else {
            KeyLayout::Raw
        }
    }

    fn backend(&self) -> &str {
        "memory"
    }

    fn version(&self) -> &str {
        env!("CARGO_PKG_VERSION")
    }

    async fn read(&self, key: &str) -> Result<Option<Vec<u8>>, BlobError> {
        let mut v = self.map().get(&self.k(key)).cloned();
        if self.defect == Some(Defect::ReadTruncates)
            && let Some(b) = v.as_mut()
        {
            b.pop();
        }
        Ok(v)
    }

    async fn write(&self, key: &str, bytes: Vec<u8>) -> Result<(), BlobError> {
        if self.defect != Some(Defect::WriteIsLost) {
            self.map().insert(self.k(key), bytes);
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
            .map(|v| Entry::new(key, v.len() as u64 + extra).with_tag(tag_of(v, self.defect))))
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

    async fn append(&self, key: &str, bytes: Vec<u8>) -> Result<(), BlobError> {
        match self.defect {
            Some(Defect::AppendOverwrites) => {
                self.map().insert(key.to_string(), bytes);
            }
            Some(Defect::AppendTearsRecords) => {
                let half = bytes.len() / 2;
                self.map()
                    .entry(key.to_string())
                    .or_default()
                    .extend_from_slice(&bytes[..half]);
                yield_now().await;
                self.map()
                    .entry(key.to_string())
                    .or_default()
                    .extend_from_slice(&bytes[half..]);
            }
            _ => {
                self.map()
                    .entry(key.to_string())
                    .or_default()
                    .extend_from_slice(&bytes);
            }
        }
        Ok(())
    }

    async fn read_range(
        &self,
        key: &str,
        offset: u64,
        len: u64,
    ) -> Result<Option<Vec<u8>>, BlobError> {
        let map = self.map();
        let Some(v) = map.get(&self.k(key)) else {
            return Ok(None);
        };
        let offset = if self.defect == Some(Defect::RangeIgnoresOffset) {
            0
        } else {
            offset
        };
        let mut out = slice_of(v, offset, len);
        if self.defect == Some(Defect::RangeShort) {
            out.pop();
        }
        Ok(Some(out))
    }

    async fn read_range_if(
        &self,
        key: &str,
        offset: u64,
        len: u64,
        tag: &str,
    ) -> Result<Option<Vec<u8>>, BlobError> {
        {
            let map = self.map();
            match map.get(&self.k(key)) {
                None => return Ok(None),
                Some(v)
                    if self.defect != Some(Defect::RangeIfIgnoresTag)
                        && tag_of(v, self.defect) != tag =>
                {
                    return Err(BlobError::Changed(key.to_string()));
                }
                Some(_) => {}
            }
        }
        self.read_range(key, offset, len).await
    }

    async fn write_stream(&self, key: &str) -> Result<Box<dyn BlobSink>, BlobError> {
        Ok(self.sink(key, false))
    }

    async fn write_stream_atomic(&self, key: &str) -> Result<Box<dyn BlobSink>, BlobError> {
        Ok(self.sink(key, true))
    }
}
