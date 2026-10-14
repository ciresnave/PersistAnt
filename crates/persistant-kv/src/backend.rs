// SPDX-License-Identifier: MIT OR Apache-2.0
use std::fmt;
use std::future::Future;
use std::time::Duration;

/// Why a key-value operation failed. Plain data.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum KvError {
    /// The backend or its driver reported a failure; the text is the driver's.
    Backend(String),
    /// The backend cannot perform this operation as its contract requires, and says so rather
    /// than approximating.
    Unsupported(String),
    /// A stored record could not be decoded.
    Corrupt(String),
}

impl fmt::Display for KvError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            KvError::Backend(m) => write!(f, "kv backend: {m}"),
            KvError::Unsupported(m) => write!(f, "kv backend does not support: {m}"),
            KvError::Corrupt(m) => write!(f, "kv record is corrupt: {m}"),
        }
    }
}

impl std::error::Error for KvError {}

impl From<persistant_blob::BlobError> for KvError {
    fn from(e: persistant_blob::BlobError) -> Self {
        match e {
            persistant_blob::BlobError::Unsupported(m) => KvError::Unsupported(m),
            other => KvError::Backend(other.to_string()),
        }
    }
}

/// One page of keys.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct KeyPage {
    /// At most `limit` keys.
    pub keys: Vec<String>,
    /// `Some(cursor)`: pass it as `after` for the next page. `None`: the listing is complete.
    pub next: Option<String>,
}

impl KeyPage {
    /// A page of `keys` with an optional continuation.
    pub fn new(keys: Vec<String>, next: Option<String>) -> Self {
        KeyPage { keys, next }
    }
}

/// A key-value store.
///
/// Small on purpose; every method is a primitive a capability's contract is written against.
/// Optional methods default to [`KvError::Unsupported`].
pub trait KvBackend: Send + Sync {
    /// The backend's name as it appears in matrix rows.
    fn backend(&self) -> &str;

    /// The backend's version string as it appears in matrix rows.
    fn version(&self) -> &str;

    /// The value at `key`, or `None` if absent or expired (`kv.get`, `kv.ttl`).
    fn get(&self, key: &str) -> impl Future<Output = Result<Option<Vec<u8>>, KvError>> + Send;

    /// Store `value` at `key`, replacing any earlier value (`kv.put`). With `ttl`, the entry is
    /// invisible to `get` and to listing once the time has passed (`kv.ttl`).
    fn put(
        &self,
        key: &str,
        value: Vec<u8>,
        ttl: Option<Duration>,
    ) -> impl Future<Output = Result<(), KvError>> + Send;

    /// Remove `key`. Removing an absent key is `Ok` (`kv.delete`).
    fn delete(&self, key: &str) -> impl Future<Output = Result<(), KvError>> + Send;

    /// Store `value` only if the key is absent **or expired**. `Ok(true)` stored it, `Ok(false)`
    /// found a live value and changed nothing. Of concurrent calls for one key exactly one gets `Ok(true)`
    /// (`kv.put_if_absent`).
    fn put_if_absent(
        &self,
        key: &str,
        value: Vec<u8>,
        ttl: Option<Duration>,
    ) -> impl Future<Output = Result<bool, KvError>> + Send {
        let _ = (key, value, ttl);
        async { Err(KvError::Unsupported("put_if_absent".to_string())) }
    }

    /// Keys beginning with the literal `prefix`: every key present for the whole listing at least
    /// once, at most `limit` per call, order unspecified, expired entries never
    /// (`kv.prefix_list`).
    fn prefix_list(
        &self,
        prefix: &str,
        after: Option<&str>,
        limit: usize,
    ) -> impl Future<Output = Result<KeyPage, KvError>> + Send {
        let _ = (prefix, after, limit);
        async { Err(KvError::Unsupported("prefix_list".to_string())) }
    }

    /// Keys beginning with `prefix` in ascending bytewise order, each once, strictly after
    /// `after`, at most `limit` per call (`kv.ordered_scan`).
    fn ordered_scan(
        &self,
        prefix: &str,
        after: Option<&str>,
        limit: usize,
    ) -> impl Future<Output = Result<KeyPage, KvError>> + Send {
        let _ = (prefix, after, limit);
        async { Err(KvError::Unsupported("ordered_scan".to_string())) }
    }

    /// If the **live** value at `key` equals `expected`, store `new` (with `ttl`) and return
    /// `Ok(true)`; an absent, expired or different value returns `Ok(false)` and changes nothing.
    /// Of concurrent callers expecting the same value exactly one gets `Ok(true)`
    /// (`kv.compare_and_swap`).
    fn compare_and_swap(
        &self,
        key: &str,
        expected: &[u8],
        new: Vec<u8>,
        ttl: Option<Duration>,
    ) -> impl Future<Output = Result<bool, KvError>> + Send {
        let _ = (key, expected, new, ttl);
        async { Err(KvError::Unsupported("compare_and_swap".to_string())) }
    }

    /// If the **live** value at `key` equals `expected`, remove it and return `Ok(true)`;
    /// otherwise `Ok(false)` and nothing changes. Of concurrent callers exactly one gets
    /// `Ok(true)` (`kv.compare_and_delete`).
    fn compare_and_delete(
        &self,
        key: &str,
        expected: &[u8],
    ) -> impl Future<Output = Result<bool, KvError>> + Send {
        let _ = (key, expected);
        async { Err(KvError::Unsupported("compare_and_delete".to_string())) }
    }
}
