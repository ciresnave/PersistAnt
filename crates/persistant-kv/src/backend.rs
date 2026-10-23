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
    /// The call did not finish within the configured per-call bound and was cut off. A batch that
    /// timed out applied nothing; a single write that timed out may or may not have taken effect.
    TimedOut(Duration),
}

impl fmt::Display for KvError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            KvError::Backend(m) => write!(f, "kv backend: {m}"),
            KvError::Unsupported(m) => write!(f, "kv backend does not support: {m}"),
            KvError::Corrupt(m) => write!(f, "kv record is corrupt: {m}"),
            KvError::TimedOut(d) => write!(f, "kv call timed out after {d:?}"),
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

/// One operation of a [`KvBackend::batch`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum BatchOp {
    /// Store `value` at `key`, with an optional time to live.
    Put {
        /// The key.
        key: String,
        /// The value.
        value: Vec<u8>,
        /// Expiry, as for [`KvBackend::put`].
        ttl: Option<Duration>,
    },
    /// Remove `key`.
    Delete {
        /// The key.
        key: String,
    },
    /// A precondition: the **live** value at `key` must equal `expected` (`None`: the key must be
    /// absent or expired) when the batch applies, or the whole batch is refused.
    Require {
        /// The key.
        key: String,
        /// The value it must hold, or `None` for absent.
        expected: Option<Vec<u8>>,
    },
}

impl BatchOp {
    /// A put without a time to live.
    pub fn put(key: impl Into<String>, value: impl Into<Vec<u8>>) -> Self {
        BatchOp::Put {
            key: key.into(),
            value: value.into(),
            ttl: None,
        }
    }

    /// A delete.
    pub fn delete(key: impl Into<String>) -> Self {
        BatchOp::Delete { key: key.into() }
    }

    /// A precondition that `key` currently holds `expected` (`None`: absent).
    pub fn require(key: impl Into<String>, expected: Option<Vec<u8>>) -> Self {
        BatchOp::Require {
            key: key.into(),
            expected,
        }
    }
}

/// One page of keys with their values, in key order.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ValuePage {
    /// At most `limit` `(key, value)` pairs, ascending by key bytes.
    pub entries: Vec<(String, Vec<u8>)>,
    /// `Some(cursor)`: pass it as `after` for the next page. `None`: the listing is complete.
    pub next: Option<String>,
}

impl ValuePage {
    /// A page of `entries` with an optional continuation.
    pub fn new(entries: Vec<(String, Vec<u8>)>, next: Option<String>) -> Self {
        ValuePage { entries, next }
    }
}

/// A key-value store.
///
/// What a backend saw when one call was deliberately blocked on a lock, for `kv.bounded_call`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundedProbe {
    /// The configured per-call bound.
    pub bound: Duration,
    /// How long the blocked call took (the test's own limit if it never finished).
    pub elapsed: Duration,
    /// Whether the blocked call returned at all.
    pub finished: bool,
    /// What it returned: `None` for `Ok`, else the error. It must be [`KvError::TimedOut`].
    pub error: Option<KvError>,
    /// Whether the blocked batch's key was visible once the block was gone (it must not be).
    pub applied_after: bool,
    /// Whether a normal batch worked once the block was gone.
    pub recovered: bool,
}

/// Small on purpose; every method is a primitive a capability's contract is written against.
/// Optional methods default to [`KvError::Unsupported`].
pub trait KvBackend: Send + Sync {
    /// The backend's name as it appears in matrix rows.
    fn backend(&self) -> &str;

    /// The backend's version string as it appears in matrix rows.
    fn version(&self) -> &str;

    /// The per-call timeout this backend enforces, or `None` if none is configured
    /// (`kv.bounded_call` is declared only for `Some`).
    fn call_bound(&self) -> Option<Duration> {
        None
    }

    /// Block a batch on a lock and report what happened (`kv.bounded_call`). The default is
    /// [`KvError::Unsupported`].
    fn bounded_call_probe(&self) -> impl Future<Output = Result<BoundedProbe, KvError>> + Send {
        async { Err(KvError::Unsupported("bounded_call_probe".to_string())) }
    }

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

    /// Remove `key` and return the **live** value it held, to exactly one of any number of concurrent
    /// callers; `None` if the key is absent or expired (`kv.take`). A single-use token is consumed
    /// with this, not with `get` then `delete`.
    ///
    /// The provided body is `get`, then `compare_and_delete` of what was read: exactly one winner is
    /// inherited from `compare_and_delete`'s own guarantee, a value that changed in between is read
    /// again, and a backend without `compare_and_delete` therefore has no `take` (it returns
    /// [`KvError::Unsupported`] for a present key). A backend may override it with one native
    /// operation (`DELETE .. RETURNING`); the conformance test is the same.
    fn take(&self, key: &str) -> impl Future<Output = Result<Option<Vec<u8>>, KvError>> + Send {
        async move {
            // Another caller rewriting the key between the two calls is rare; a key rewritten this
            // many times in a row is not a single-use token, so say so rather than spin.
            for _ in 0..16 {
                let Some(value) = self.get(key).await? else {
                    return Ok(None);
                };
                if self.compare_and_delete(key, &value).await? {
                    return Ok(Some(value));
                }
            }
            Err(KvError::Backend(
                "take: the value changed on every attempt".to_string(),
            ))
        }
    }

    /// The time to live left on a **live** entry: `Some(d)` with `0 < d <= ttl` and `d` falling as
    /// time passes; `None` for an absent or expired entry or one written without a time to live
    /// (`kv.ttl_remaining`).
    fn ttl_remaining(
        &self,
        key: &str,
    ) -> impl Future<Output = Result<Option<Duration>, KvError>> + Send {
        let _ = key;
        async { Err(KvError::Unsupported("ttl_remaining".to_string())) }
    }

    /// Physically remove expired entries and return how many were removed. Visibility is
    /// already correct without it (expired entries are invisible to `get` and listing); this is
    /// housekeeping. A live entry is never removed (`kv.purge_expired`).
    fn purge_expired(&self) -> impl Future<Output = Result<u64, KvError>> + Send {
        async { Err(KvError::Unsupported("purge_expired".to_string())) }
    }

    /// Apply `ops` as one unit. `Ok(true)`: every `Put` and `Delete` took effect together;
    /// `Ok(false)`: a `Require` was not met and **nothing changed**; `Err`: an operation failed
    /// and nothing changed. Of concurrent batches requiring the same value, exactly one applies
    /// (`kv.batch_atomic.rollback`). A backend with no multi-key transaction returns
    /// [`KvError::Unsupported`]; the default does.
    fn batch(&self, ops: &[BatchOp]) -> impl Future<Output = Result<bool, KvError>> + Send {
        let _ = ops;
        async { Err(KvError::Unsupported("batch".to_string())) }
    }

    /// As [`KvBackend::ordered_scan`], with each key's value, from **one consistent snapshot**:
    /// a concurrent [`KvBackend::batch`] is seen whole or not at all
    /// (`kv.ordered_scan_values`, `kv.batch_atomic.concurrent_read`).
    fn ordered_scan_values(
        &self,
        prefix: &str,
        after: Option<&str>,
        limit: usize,
    ) -> impl Future<Output = Result<ValuePage, KvError>> + Send {
        let _ = (prefix, after, limit);
        async { Err(KvError::Unsupported("ordered_scan_values".to_string())) }
    }
}
