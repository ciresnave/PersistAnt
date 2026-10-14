// SPDX-License-Identifier: MIT OR Apache-2.0
use crate::{KeyPage, KvBackend, KvError};
use persistant_blob::{BlobBackend, BlobError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// A [`KvBackend`] built on any [`BlobBackend`].
///
/// A **wrapper type, not a blanket impl** (Rust has no specialisation, and a blanket impl would
/// forbid a native KV backend for the same type). Each record is stored under `kv/<key>` as an
/// envelope: one version byte, eight bytes of expiry (milliseconds since the Unix epoch, `0` for
/// none) and the value.
///
/// What it gives is *eligible by derivation* and then **tested** for each blob backend, version
/// and config:
///
/// - `get`, `put`, `delete`, `prefix_list`, `ordered_scan` need `blob.read`, `blob.write`,
///   `blob.delete`, `blob.list`. `put` uses `replace_atomic`; where the blob backend has none it
///   is refused, unless the store was built with [`KvOnBlob::with_in_place_put`].
/// - `put_if_absent` needs `blob.create_exclusive`. An existing entry that has **expired** cannot
///   be reclaimed safely without compare-and-delete (delete-then-create lets a second reclaimer
///   delete the first one's fresh entry), so that case returns [`KvError::Unsupported`] rather
///   than guess, and `put_if_absent` with a time to live is refused.
/// - `compare_and_swap`, `take` and `batch_atomic` are not offered: a blob store has no
///   compare-and-delete or multi-key transaction.
#[derive(Debug)]
pub struct KvOnBlob<B> {
    blob: B,
    in_place_put: bool,
    name: String,
    version: String,
}

const PREFIX: &str = "kv/";
const VERSION_BYTE: u8 = 1;

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

fn encode(value: &[u8], ttl: Option<Duration>) -> Vec<u8> {
    let expiry = ttl.map_or(0, |d| now_ms().saturating_add(d.as_millis().max(1) as u64));
    let mut out = Vec::with_capacity(9 + value.len());
    out.push(VERSION_BYTE);
    out.extend_from_slice(&expiry.to_be_bytes());
    out.extend_from_slice(value);
    out
}

/// `Ok(None)` means expired.
fn decode(bytes: Vec<u8>) -> Result<Option<Vec<u8>>, KvError> {
    if bytes.len() < 9 || bytes[0] != VERSION_BYTE {
        return Err(KvError::Corrupt(format!(
            "{} bytes, version byte {:?}",
            bytes.len(),
            bytes.first()
        )));
    }
    let mut ms = [0u8; 8];
    ms.copy_from_slice(&bytes[1..9]);
    let expiry = u64::from_be_bytes(ms);
    if expiry != 0 && now_ms() >= expiry {
        return Ok(None);
    }
    Ok(Some(bytes[9..].to_vec()))
}

fn expired(bytes: &[u8]) -> bool {
    matches!(decode(bytes.to_vec()), Ok(None))
}

impl<B: BlobBackend> KvOnBlob<B> {
    /// Build a key-value store on `blob`. **Strict**: `put` needs the blob store's
    /// `replace_atomic`, and returns [`KvError::Unsupported`] where it has none, rather than
    /// write in place and let a reader see a half-written value.
    pub fn new(blob: B) -> Self {
        Self::build(blob, false)
    }

    /// As [`KvOnBlob::new`], but `put` falls back to the blob store's plain `write` where it has
    /// no `replace_atomic`. A concurrent `get` can then see a partly written record (reported as
    /// [`KvError::Corrupt`]). Chosen explicitly, and recorded as the config fact
    /// `put_mode=in_place` so a matrix row says which mode it was certified under.
    pub fn with_in_place_put(blob: B) -> Self {
        Self::build(blob, true)
    }

    fn build(blob: B, in_place_put: bool) -> Self {
        let name = format!("kv-on-blob:{}", blob.backend());
        // Major.minor only (pre-1.0 the second number is the major): a patch bump does not
        // re-key every recorded row, a breaking change to the adapter does.
        let version = format!(
            "{}.{}+{}",
            env!("CARGO_PKG_VERSION_MAJOR"),
            env!("CARGO_PKG_VERSION_MINOR"),
            blob.version()
        );
        KvOnBlob {
            blob,
            in_place_put,
            name,
            version,
        }
    }

    /// The config facts a matrix row for this adapter is recorded under (merge them with the blob
    /// backend's own facts).
    pub fn config(&self) -> persistant_core::Config {
        persistant_core::Config::new().with(
            "put_mode",
            if self.in_place_put {
                "in_place"
            } else {
                "atomic"
            },
        )
    }

    /// The underlying blob backend.
    pub fn blob(&self) -> &B {
        &self.blob
    }

    fn blob_key(key: &str) -> String {
        format!("{PREFIX}{key}")
    }

    async fn scan(
        &self,
        prefix: &str,
        after: Option<&str>,
        limit: usize,
    ) -> Result<KeyPage, KvError> {
        let want = limit;
        let bprefix = Self::blob_key(prefix);
        let mut cursor = after.map(Self::blob_key);
        let mut keys: Vec<String> = Vec::new();
        loop {
            // Ask for one more than still needed so a full page can tell whether more follow.
            let ask = want.saturating_sub(keys.len()).saturating_add(1);
            let page = self.blob.list(&bprefix, cursor.as_deref(), ask).await?;
            let exhausted = page.next.is_none();
            let last = page.entries.last().map(|e| e.key.clone());
            for e in page.entries {
                if keys.len() == want {
                    // A live key beyond the page exists, so a continuation is due.
                    let next = keys.last().cloned();
                    return Ok(KeyPage::new(keys, next));
                }
                match self.blob.read(&e.key).await? {
                    Some(bytes) if !expired(&bytes) => {
                        keys.push(e.key[PREFIX.len()..].to_string());
                    }
                    _ => {}
                }
            }
            if exhausted {
                return Ok(KeyPage::new(keys, None));
            }
            cursor = last;
        }
    }
}

impl<B: BlobBackend> KvBackend for KvOnBlob<B> {
    fn backend(&self) -> &str {
        &self.name
    }

    fn version(&self) -> &str {
        &self.version
    }

    async fn get(&self, key: &str) -> Result<Option<Vec<u8>>, KvError> {
        match self.blob.read(&Self::blob_key(key)).await? {
            Some(bytes) => decode(bytes),
            None => Ok(None),
        }
    }

    async fn put(&self, key: &str, value: Vec<u8>, ttl: Option<Duration>) -> Result<(), KvError> {
        let k = Self::blob_key(key);
        let env = encode(&value, ttl);
        match self.blob.replace_atomic(&k, env.clone()).await {
            Ok(()) => Ok(()),
            Err(BlobError::Unsupported(_)) if self.in_place_put => {
                Ok(self.blob.write(&k, env).await?)
            }
            Err(e) => Err(e.into()),
        }
    }

    async fn delete(&self, key: &str) -> Result<(), KvError> {
        Ok(self.blob.delete(&Self::blob_key(key)).await?)
    }

    async fn put_if_absent(
        &self,
        key: &str,
        value: Vec<u8>,
        ttl: Option<Duration>,
    ) -> Result<bool, KvError> {
        if ttl.is_some() {
            return Err(KvError::Unsupported(
                "put_if_absent with a time to live needs compare-and-delete to reclaim".to_string(),
            ));
        }
        let k = Self::blob_key(key);
        if self.blob.create_exclusive(&k, encode(&value, None)).await? {
            return Ok(true);
        }
        match self.blob.read(&k).await? {
            Some(bytes) if expired(&bytes) => Err(KvError::Unsupported(
                "reclaiming an expired entry needs compare-and-delete".to_string(),
            )),
            _ => Ok(false),
        }
    }

    async fn prefix_list(
        &self,
        prefix: &str,
        after: Option<&str>,
        limit: usize,
    ) -> Result<KeyPage, KvError> {
        self.scan(prefix, after, limit).await
    }

    async fn ordered_scan(
        &self,
        prefix: &str,
        after: Option<&str>,
        limit: usize,
    ) -> Result<KeyPage, KvError> {
        self.scan(prefix, after, limit).await
    }
}
