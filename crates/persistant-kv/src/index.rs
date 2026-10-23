// SPDX-License-Identifier: MIT OR Apache-2.0
//! Secondary-key lookup as a layer over any [`KvBackend`] (`docs/DESIGN-AUTH-STORAGE.md` section 4.3).
//!
//! A secondary index is not something a backend offers; it is a discipline over [`KvBackend::batch`]:
//! a record and its index entries are written, moved and removed in **one** batch, so a crash or a
//! concurrent update never leaves a record without its index entries or the reverse. An index entry is a
//! key the caller names (`user:7:tok-1`), whose value is the primary key it points at; looking an index
//! up scans the entries under a prefix from one snapshot ([`KvBackend::ordered_scan_values`]) and reads
//! each primary record.
//!
//! The layer exists for a backend that has `kv.batch_atomic` and `kv.ordered_scan_values` (memory and the
//! SQL engines) and returns [`KvError::Unsupported`] on one that has not (a blob store): there is no
//! way to keep a record and its index together there, and an index that can diverge is worse than none.
//! The conformance leaf is `kv.index`.
//!
//! All the keys of one record carry the **same time to live**, so they expire together and a count of
//! the primary keys (`kv.count`) is a count of live records.

use crate::{BatchOp, KvBackend, KvError};
use std::time::Duration;

/// Records with secondary index entries, on a borrowed [`KvBackend`].
#[derive(Debug)]
pub struct Indexed<'a, K> {
    kv: &'a K,
}

impl<K> Clone for Indexed<'_, K> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<K> Copy for Indexed<'_, K> {}

/// Entries per page when scanning an index.
const PAGE: usize = 500;

fn put(key: &str, value: Vec<u8>, ttl: Option<Duration>) -> BatchOp {
    BatchOp::Put {
        key: key.to_string(),
        value,
        ttl,
    }
}

impl<'a, K: KvBackend> Indexed<'a, K> {
    /// Index records on `kv`.
    pub fn new(kv: &'a K) -> Self {
        Self { kv }
    }

    /// The backend underneath, for the reads and writes that need no index.
    pub fn kv(&self) -> &'a K {
        self.kv
    }

    /// Store a new record at `primary` and an entry at each of `indexes`, together. `Ok(false)`: a
    /// record already lives at `primary` and nothing changed (use [`replace`](Self::replace)).
    pub async fn insert(
        &self,
        primary: &str,
        value: Vec<u8>,
        indexes: &[&str],
        ttl: Option<Duration>,
    ) -> Result<bool, KvError> {
        let mut ops = vec![BatchOp::require(primary, None), put(primary, value, ttl)];
        ops.extend(
            indexes
                .iter()
                .map(|i| put(i, primary.as_bytes().to_vec(), ttl)),
        );
        self.kv.batch(&ops).await
    }

    /// Replace the record at `primary` **if it still holds `expected`**, moving its index entries from
    /// `old` to `new` in the same batch. `Ok(false)`: it did not, nothing changed; read it again. Of
    /// concurrent callers expecting the same value exactly one gets `Ok(true)`.
    pub async fn replace(
        &self,
        primary: &str,
        expected: &[u8],
        value: Vec<u8>,
        old: &[&str],
        new: &[&str],
        ttl: Option<Duration>,
    ) -> Result<bool, KvError> {
        let mut ops = vec![BatchOp::require(primary, Some(expected.to_vec()))];
        ops.extend(
            old.iter()
                .filter(|o| !new.contains(o))
                .map(|o| BatchOp::delete(*o)),
        );
        ops.push(put(primary, value, ttl));
        ops.extend(new.iter().map(|n| put(n, primary.as_bytes().to_vec(), ttl)));
        self.kv.batch(&ops).await
    }

    /// Remove the record at `primary` and its entries `indexes`, **if it still holds `expected`**.
    /// `Ok(false)`: it did not, nothing changed; read it again (the index keys are computed from the
    /// value, so a changed value may have changed them).
    pub async fn remove(
        &self,
        primary: &str,
        expected: &[u8],
        indexes: &[&str],
    ) -> Result<bool, KvError> {
        let mut ops = vec![
            BatchOp::require(primary, Some(expected.to_vec())),
            BatchOp::delete(primary),
        ];
        ops.extend(indexes.iter().map(|i| BatchOp::delete(*i)));
        self.kv.batch(&ops).await
    }

    /// The record one index entry points at, as `(primary key, value)`; `None` if there is no such
    /// entry or its record has gone.
    pub async fn lookup_one(&self, index: &str) -> Result<Option<(String, Vec<u8>)>, KvError> {
        let Some(pointer) = self.kv.get(index).await? else {
            return Ok(None);
        };
        let primary = primary_of(&pointer)?;
        Ok(self.kv.get(&primary).await?.map(|v| (primary, v)))
    }

    /// Every record pointed at by an entry under `index_prefix`, as `(primary key, value)` in the
    /// entries' key order. The entries are read from one snapshot; each record is read after, so one
    /// removed in between is left out rather than reported as an error.
    pub async fn lookup(&self, index_prefix: &str) -> Result<Vec<(String, Vec<u8>)>, KvError> {
        let mut out = Vec::new();
        let mut after: Option<String> = None;
        loop {
            let page = self
                .kv
                .ordered_scan_values(index_prefix, after.as_deref(), PAGE)
                .await?;
            for (_, pointer) in &page.entries {
                let primary = primary_of(pointer)?;
                if let Some(v) = self.kv.get(&primary).await? {
                    out.push((primary, v));
                }
            }
            match page.next {
                Some(cursor) => after = Some(cursor),
                None => return Ok(out),
            }
        }
    }
}

fn primary_of(pointer: &[u8]) -> Result<String, KvError> {
    String::from_utf8(pointer.to_vec())
        .map_err(|_| KvError::Corrupt("an index entry does not hold a UTF-8 key".to_string()))
}
