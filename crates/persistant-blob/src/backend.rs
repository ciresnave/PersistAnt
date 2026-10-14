// SPDX-License-Identifier: MIT OR Apache-2.0
use std::fmt;
use std::future::Future;

/// Why a blob operation failed. Plain data, no boxed payloads.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum BlobError {
    /// The backend or its driver reported a failure; the text is the driver's.
    Backend(String),
    /// The backend cannot perform this operation as its contract requires, and says so rather
    /// than approximating (for example an exclusive create that is check-then-write).
    Unsupported(String),
}

impl fmt::Display for BlobError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BlobError::Backend(m) => write!(f, "blob backend: {m}"),
            BlobError::Unsupported(m) => write!(f, "blob backend does not support: {m}"),
        }
    }
}

impl std::error::Error for BlobError {}

/// What is stored at one key.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Entry {
    /// The key.
    pub key: String,
    /// Size of the value in bytes.
    pub len: u64,
}

impl Entry {
    /// An entry for `key` holding `len` bytes.
    pub fn new(key: impl Into<String>, len: u64) -> Self {
        Entry {
            key: key.into(),
            len,
        }
    }
}

/// One page of a listing.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Page {
    /// At most `limit` entries.
    pub entries: Vec<Entry>,
    /// `Some(cursor)`: pass it as `after` for the next page. `None`: the listing is complete.
    pub next: Option<String>,
}

impl Page {
    /// A page of `entries` with an optional continuation.
    pub fn new(entries: Vec<Entry>, next: Option<String>) -> Self {
        Page { entries, next }
    }
}

/// A blob store: bytes by key.
///
/// Small on purpose. Every method is a primitive a capability's contract is written against; a
/// method a backend cannot honour must fail with [`BlobError`] rather than approximate, and the
/// capability stays off because its test did not pass. Missing keys are `Ok(None)`, not errors.
pub trait BlobBackend: Send + Sync {
    /// The backend's name as it appears in matrix rows (`memory`, `fs`, `s3`).
    fn backend(&self) -> &str;

    /// The backend's version string as it appears in matrix rows.
    fn version(&self) -> &str;

    /// The value at `key`, or `None` if absent.
    fn read(&self, key: &str) -> impl Future<Output = Result<Option<Vec<u8>>, BlobError>> + Send;

    /// Store `bytes` at `key`, replacing any earlier value. Atomicity is **not** implied; that is
    /// `blob.replace_atomic`.
    fn write(
        &self,
        key: &str,
        bytes: Vec<u8>,
    ) -> impl Future<Output = Result<(), BlobError>> + Send;

    /// Remove `key`. Removing an absent key is `Ok`.
    fn delete(&self, key: &str) -> impl Future<Output = Result<(), BlobError>> + Send;

    /// The entry at `key` without reading its value, or `None` if absent.
    fn stat(&self, key: &str) -> impl Future<Output = Result<Option<Entry>, BlobError>> + Send;

    /// Up to `limit` entries whose key starts with the literal `prefix`, in ascending bytewise
    /// key order, strictly after `after` if given.
    fn list(
        &self,
        prefix: &str,
        after: Option<&str>,
        limit: usize,
    ) -> impl Future<Output = Result<Page, BlobError>> + Send;

    /// Write `bytes` at `key` only if nothing is there. `Ok(true)` created it, `Ok(false)` found
    /// a value and changed nothing. Of any number of concurrent callers for one key, exactly one
    /// gets `Ok(true)` (`blob.create_exclusive`). A backend that cannot promise that returns
    /// [`BlobError::Unsupported`]; the default does.
    fn create_exclusive(
        &self,
        key: &str,
        bytes: Vec<u8>,
    ) -> impl Future<Output = Result<bool, BlobError>> + Send {
        let _ = (key, bytes);
        async { Err(BlobError::Unsupported("create_exclusive".to_string())) }
    }

    /// Replace the value at `key` so that a concurrent reader sees the whole old value or the
    /// whole new one, never a mix (`blob.replace_atomic.concurrent_read`). A backend that cannot
    /// promise that returns [`BlobError::Unsupported`]; the default does.
    fn replace_atomic(
        &self,
        key: &str,
        bytes: Vec<u8>,
    ) -> impl Future<Output = Result<(), BlobError>> + Send {
        let _ = (key, bytes);
        async { Err(BlobError::Unsupported("replace_atomic".to_string())) }
    }

    /// Add `bytes` after the existing bytes of `key`, in call order (`blob.append`). Of
    /// concurrent calls, each call's bytes arrive whole and exactly once
    /// (`blob.append.concurrent_whole`). A backend that cannot promise that returns
    /// [`BlobError::Unsupported`]; the default does.
    fn append(
        &self,
        key: &str,
        bytes: Vec<u8>,
    ) -> impl Future<Output = Result<(), BlobError>> + Send {
        let _ = (key, bytes);
        async { Err(BlobError::Unsupported("append".to_string())) }
    }
}
