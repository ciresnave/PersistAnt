// SPDX-License-Identifier: MIT OR Apache-2.0
use std::fmt;
use std::future::Future;
use std::pin::Pin;

/// Why a blob operation failed. Plain data, no boxed payloads.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum BlobError {
    /// The backend or its driver reported a failure; the text is the driver's.
    Backend(String),
    /// The backend cannot perform this operation as its contract requires, and says so rather
    /// than approximating (for example an exclusive create that is check-then-write).
    Unsupported(String),
    /// The stored value is not the version the caller named (`read_range_if`): it was replaced
    /// between the caller's `stat` and this read. The text names the key, never the bytes.
    Changed(String),
}

impl fmt::Display for BlobError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BlobError::Backend(m) => write!(f, "blob backend: {m}"),
            BlobError::Unsupported(m) => write!(f, "blob backend does not support: {m}"),
            BlobError::Changed(m) => write!(f, "blob value changed under the reader: {m}"),
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
    /// The backend's token for this version of the value (an etag, a content hash), or `None` if it
    /// has none. Two reads of one key with the same tag saw the same value; `stat` carries it so a
    /// multi-range reader can use [`BlobBackend::read_range_if`].
    pub tag: Option<String>,
}

impl Entry {
    /// An entry for `key` holding `len` bytes.
    pub fn new(key: impl Into<String>, len: u64) -> Self {
        Entry {
            key: key.into(),
            len,
            tag: None,
        }
    }

    /// This entry with the backend's version token set.
    pub fn with_tag(mut self, tag: impl Into<String>) -> Self {
        self.tag = Some(tag.into());
        self
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

/// How a layer above this store (the key-value adapter) should turn its keys into blob keys.
///
/// A blob key is a path on a filesystem backend, and a filesystem is not a faithful key space: some
/// ignore case, some forbid names, and a file cannot also be a directory. A backend that is not
/// faithful says so here, and the layer above encodes its keys to fit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum KeyLayout {
    /// The key is used as it is. Right for a store whose keys are opaque strings.
    Raw,
    /// The key's bytes are written as lower-case hexadecimal, split into path segments of at most
    /// 100 characters, ending in a fixed final segment. Safe on any filesystem: only `0-9 a-f`, `/`
    /// and `-` appear, so no two keys share a file whatever the case rules, no name is reserved,
    /// and no key is another key's directory. Bytewise key order and the prefix relation are kept,
    /// so listing by prefix and in order still work.
    HexPath,
}

/// A boxed future, for the object-safe [`BlobSink`].
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A streamed write in progress (`blob.write_stream`, `blob.write_stream.atomic`).
///
/// Nothing is visible at the key until [`finish`](BlobSink::finish) returns `Ok`. [`abort`] or
/// dropping the sink leaves the key as it was and leaves no staging that a `list` can see. `write`
/// takes a borrowed slice so a caller reuses one buffer; the backend copies what it keeps.
///
/// [`abort`]: BlobSink::abort
pub trait BlobSink: Send {
    /// Add `chunk` to the end of the value being written.
    fn write<'a>(&'a mut self, chunk: &'a [u8]) -> BoxFuture<'a, Result<(), BlobError>>;

    /// Publish the value at the key. For a sink from `write_stream_atomic`, a concurrent reader sees
    /// the whole old value or the whole new one, never a mix.
    fn finish(self: Box<Self>) -> BoxFuture<'static, Result<(), BlobError>>;

    /// Discard what was written; the key is as it was before the stream began.
    fn abort(self: Box<Self>) -> BoxFuture<'static, Result<(), BlobError>>;
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

    /// How keys built on this store should be laid out (default [`KeyLayout::Raw`]).
    fn key_layout(&self) -> KeyLayout {
        KeyLayout::Raw
    }

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

    /// The bytes `[offset, offset + len)` of `key`, or `None` if it is absent (`blob.read_range`).
    /// A range running past the end returns the bytes that exist, possibly none; `len == 0` on a
    /// present key returns an empty vector. A backend that cannot read a range without reading the
    /// rest returns [`BlobError::Unsupported`]; the default does.
    fn read_range(
        &self,
        key: &str,
        offset: u64,
        len: u64,
    ) -> impl Future<Output = Result<Option<Vec<u8>>, BlobError>> + Send {
        let _ = (key, offset, len);
        async { Err(BlobError::Unsupported("read_range".to_string())) }
    }

    /// As [`read_range`](BlobBackend::read_range), but only if the stored value still has `tag`
    /// (from [`Entry::tag`]); otherwise [`BlobError::Changed`] (`blob.read_range.consistent`). A
    /// backend with no version token returns [`BlobError::Unsupported`]; the default does.
    fn read_range_if(
        &self,
        key: &str,
        offset: u64,
        len: u64,
        tag: &str,
    ) -> impl Future<Output = Result<Option<Vec<u8>>, BlobError>> + Send {
        let _ = (key, offset, len, tag);
        async { Err(BlobError::Unsupported("read_range_if".to_string())) }
    }

    /// Begin writing `key` in chunks (`blob.write_stream`). Nothing is visible at `key` until the
    /// sink's `finish`. The default returns [`BlobError::Unsupported`].
    fn write_stream(
        &self,
        key: &str,
    ) -> impl Future<Output = Result<Box<dyn BlobSink>, BlobError>> + Send {
        let _ = key;
        async { Err(BlobError::Unsupported("write_stream".to_string())) }
    }

    /// As [`write_stream`](BlobBackend::write_stream), and `finish` replaces the value so that a
    /// concurrent reader sees the whole old value or the whole new one (`blob.write_stream.atomic`).
    /// The default returns [`BlobError::Unsupported`].
    fn write_stream_atomic(
        &self,
        key: &str,
    ) -> impl Future<Output = Result<Box<dyn BlobSink>, BlobError>> + Send {
        let _ = key;
        async { Err(BlobError::Unsupported("write_stream_atomic".to_string())) }
    }
}
