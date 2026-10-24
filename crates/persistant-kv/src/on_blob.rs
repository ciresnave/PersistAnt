// SPDX-License-Identifier: MIT OR Apache-2.0
use crate::{KeyPage, KvBackend, KvError};
use persistant_blob::{BlobBackend, BlobError, KeyLayout};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// A [`KvBackend`] built on any [`BlobBackend`].
///
/// A **wrapper type, not a blanket impl** (Rust has no specialisation, and a blanket impl would
/// forbid a native KV backend for the same type). Each record is stored as an envelope: one
/// version byte, eight bytes of expiry (milliseconds since the Unix epoch, `0` for none) and the
/// value. The blob key follows the blob store's [`KeyLayout`]: `kv/<key>` for [`KeyLayout::Raw`],
/// and for [`KeyLayout::HexPath`] (a filesystem) the key's bytes in lower-case hex, in path
/// segments of at most 100 characters, ending in `/-`, under `kvx/`. The hex form keeps
/// `Key` and `key` apart on a filesystem that ignores case, cannot collide with a reserved name,
/// never makes one key another's directory, and keeps bytewise order and prefixes. A store that
/// wrote the raw form on such a backend moves its keys with [`KvOnBlob::migrate_raw_layout`].
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
    layout: KeyLayout,
    name: String,
    version: String,
}

const PREFIX: &str = "kv/";
const HEX_PREFIX: &str = "kvx/";
/// Hex characters per path segment (50 key bytes): a file name stays well under every
/// filesystem's limit of 255.
const HEX_SEGMENT: usize = 100;
/// The final segment of every encoded key. It sorts before every hex digit, so a key sorts before
/// the keys it is a prefix of even where a segment boundary falls at its end.
const HEX_END: &str = "-";

/// `key`'s bytes as lower-case hex, in segments joined by `/` (no end marker).
fn hex_segments(key: &str) -> String {
    let mut out = String::with_capacity(key.len() * 2 + key.len() / (HEX_SEGMENT / 2) + 1);
    for (i, b) in key.bytes().enumerate() {
        if i > 0 && i % (HEX_SEGMENT / 2) == 0 {
            out.push('/');
        }
        out.push(char::from_digit(u32::from(b >> 4), 16).unwrap_or('0'));
        out.push(char::from_digit(u32::from(b & 15), 16).unwrap_or('0'));
    }
    out
}

/// The key a hex blob key (without its `kvx/`) encodes, or `None` if it is not one of ours.
fn unhex(rest: &str) -> Option<String> {
    let body = rest.strip_suffix(HEX_END)?;
    let body = match body.strip_suffix('/') {
        Some(b) => b,
        None if body.is_empty() => body,
        None => return None,
    };
    let digits: Vec<u8> = body.bytes().filter(|b| *b != b'/').collect();
    if !digits.len().is_multiple_of(2) {
        return None;
    }
    let nibble = |b: u8| {
        char::from(b)
            .to_digit(16)
            .and_then(|d| u8::try_from(d).ok())
    };
    let bytes: Option<Vec<u8>> = digits
        .chunks(2)
        .map(|p| Some(nibble(p[0])? << 4 | nibble(p[1])?))
        .collect();
    // Only the canonical spelling is ours (lower-case, segments where they belong).
    String::from_utf8(bytes?)
        .ok()
        .filter(|k| hex_end(k) == rest)
}

/// `hex_segments(key)` and the end marker.
fn hex_end(key: &str) -> String {
    let h = hex_segments(key);
    if h.is_empty() {
        HEX_END.to_string()
    } else {
        format!("{h}/{HEX_END}")
    }
}
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

/// Remaining time to live of a stored envelope: `None` if it has none or has expired.
fn remaining(bytes: &[u8]) -> Result<Option<Duration>, KvError> {
    if bytes.len() < 9 || bytes[0] != VERSION_BYTE {
        return Err(KvError::Corrupt("bad envelope".to_string()));
    }
    let mut ms = [0u8; 8];
    ms.copy_from_slice(&bytes[1..9]);
    let expiry = u64::from_be_bytes(ms);
    if expiry == 0 {
        return Ok(None);
    }
    Ok(expiry
        .checked_sub(now_ms())
        .filter(|d| *d > 0)
        .map(Duration::from_millis))
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
            layout: blob.key_layout(),
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

    fn blob_key(&self, key: &str) -> String {
        match self.layout {
            KeyLayout::HexPath => format!("{HEX_PREFIX}{}", hex_end(key)),
            _ => format!("{PREFIX}{key}"),
        }
    }

    /// The blob prefix that every blob key of a key starting with `prefix` starts with.
    fn blob_prefix(&self, prefix: &str) -> String {
        match self.layout {
            KeyLayout::HexPath => format!("{HEX_PREFIX}{}", hex_segments(prefix)),
            _ => format!("{PREFIX}{prefix}"),
        }
    }

    /// The key a listed blob key stands for, or `None` for a file that is not one of ours.
    fn key_of(&self, blob_key: &str) -> Option<String> {
        match self.layout {
            KeyLayout::HexPath => unhex(blob_key.strip_prefix(HEX_PREFIX)?),
            _ => blob_key.strip_prefix(PREFIX).map(str::to_string),
        }
    }

    /// Move every record written under the raw layout (`kv/<key>`) to this store's layout. A
    /// no-op (returning 0) for a store whose layout is raw. Safe to repeat and to run beside other
    /// users of the store: a record is copied with an exclusive create, so a newer value already at
    /// the new key is never overwritten, and the old copy is removed only after the new one exists.
    /// Expired records are dropped. Returns how many live records moved.
    pub async fn migrate_raw_layout(&self) -> Result<usize, KvError> {
        if matches!(self.layout, KeyLayout::Raw) {
            return Ok(0);
        }
        let mut moved = 0;
        let mut after: Option<String> = None;
        loop {
            let page = self.blob.list(PREFIX, after.as_deref(), 100).await?;
            for e in &page.entries {
                let Some(bytes) = self.blob.read(&e.key).await? else {
                    continue;
                };
                let key = e.key[PREFIX.len()..].to_string();
                if !expired(&bytes) {
                    self.blob
                        .create_exclusive(&self.blob_key(&key), bytes)
                        .await?;
                    moved += 1;
                }
                self.blob.delete(&e.key).await?;
            }
            match page.next {
                Some(next) => after = Some(next),
                None => return Ok(moved),
            }
        }
    }

    async fn scan(
        &self,
        prefix: &str,
        after: Option<&str>,
        limit: usize,
    ) -> Result<KeyPage, KvError> {
        let want = limit;
        let bprefix = self.blob_prefix(prefix);
        let mut cursor = after.map(|a| self.blob_key(a));
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
                let Some(key) = self.key_of(&e.key) else {
                    continue;
                };
                match self.blob.read(&e.key).await? {
                    Some(bytes) if !expired(&bytes) => keys.push(key),
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
        match self.blob.read(&self.blob_key(key)).await? {
            Some(bytes) => decode(bytes),
            None => Ok(None),
        }
    }

    async fn put(&self, key: &str, value: Vec<u8>, ttl: Option<Duration>) -> Result<(), KvError> {
        let k = self.blob_key(key);
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
        Ok(self.blob.delete(&self.blob_key(key)).await?)
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
        let k = self.blob_key(key);
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

    async fn ttl_remaining(&self, key: &str) -> Result<Option<Duration>, KvError> {
        match self.blob.read(&self.blob_key(key)).await? {
            Some(bytes) => remaining(&bytes),
            None => Ok(None),
        }
    }

    // `purge_expired` is not offered: deleting an expired entry races with a concurrent `put`
    // that has just replaced it, and a blob store has no compare-and-delete to make that safe.
}

#[cfg(test)]
mod layout_tests {
    use super::*;
    use persistant_blob::{Defect, MemoryBlob};

    /// Keys that a filesystem treats badly if used as they are.
    fn awkward() -> Vec<String> {
        let mut v: Vec<String> = [
            "",
            "a",
            "A",
            "a/b",
            "a/",
            "/a",
            "..",
            "../x",
            "a/../b",
            "CON",
            "con",
            "nul.txt",
            "trailing.",
            "trailing ",
            "a:b",
            r"a\b",
            "caf\u{e9}",
            "CAF\u{c9}",
            "\u{1f600}",
            "k\0ey",
            "100%",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        // Around the segment boundary (50 key bytes), and well beyond it.
        for n in [49, 50, 51, 99, 100, 101, 150, 300] {
            v.push("x".repeat(n));
            v.push(format!("{}y", "x".repeat(n)));
            v.push("X".repeat(n));
        }
        v
    }

    fn hex_store() -> KvOnBlob<MemoryBlob> {
        KvOnBlob::new(MemoryBlob::new().with_hex_layout())
    }

    #[test]
    fn every_key_round_trips() {
        for k in awkward() {
            let enc = hex_end(&k);
            assert_eq!(unhex(&enc).as_deref(), Some(k.as_str()), "{k:?} -> {enc}");
        }
    }

    #[test]
    fn encoded_names_use_only_hex_slash_and_dash_and_stay_short() {
        for k in awkward() {
            let enc = hex_end(&k);
            assert!(
                enc.bytes().all(|b| b.is_ascii_digit()
                    || (b'a'..=b'f').contains(&b)
                    || b == b'/'
                    || b == b'-'),
                "{enc}"
            );
            assert!(
                enc.split('/').all(|s| s.len() <= HEX_SEGMENT.max(1)),
                "{enc}"
            );
        }
    }

    #[test]
    fn no_two_keys_collide_even_ignoring_case_and_none_is_anothers_directory() {
        let keys = awkward();
        let encoded: Vec<String> = keys.iter().map(|k| hex_end(k)).collect();
        for (i, a) in encoded.iter().enumerate() {
            for (j, b) in encoded.iter().enumerate() {
                if i == j {
                    continue;
                }
                assert_ne!(
                    a.to_lowercase(),
                    b.to_lowercase(),
                    "{:?} vs {:?}",
                    keys[i],
                    keys[j]
                );
                assert!(
                    !b.starts_with(&format!("{a}/")),
                    "{:?} would be a directory inside {:?}",
                    keys[j],
                    keys[i]
                );
            }
        }
        // Control: the raw form does collide ("Key" and "key" under case folding; "a" holds "a/b").
        assert_eq!("kv/Key".to_lowercase(), "kv/key");
    }

    #[test]
    fn bytewise_order_and_prefixes_survive_the_encoding() {
        let mut keys = awkward();
        keys.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
        keys.dedup();
        let mut by_encoding = keys.clone();
        by_encoding.sort_by_key(|k| hex_end(k));
        assert_eq!(
            by_encoding, keys,
            "sorting the encoded names sorts the keys"
        );
        for p in &keys {
            let pre = hex_segments(p);
            for k in &keys {
                assert_eq!(
                    hex_end(k).starts_with(&pre),
                    k.starts_with(p.as_str()),
                    "prefix {p:?} of {k:?}"
                );
            }
        }
    }

    #[test]
    fn a_name_that_is_not_ours_is_not_a_key() {
        for foreign in [
            "", "zz/-", "6/-", "61", "61/", "4A/-", "ff/-", "61//-", "/-",
        ] {
            assert_eq!(unhex(foreign), None, "{foreign:?}");
        }
        assert_eq!(unhex("-").as_deref(), Some(""), "control: the empty key");
        assert_eq!(unhex("61/-").as_deref(), Some("a"), "control: a plain key");
    }

    #[tokio::test]
    async fn keys_stay_apart_when_the_store_folds_case() {
        // The blob store ignores case, as NTFS does; the hex layout keeps `Key` and `key` apart.
        let kv = KvOnBlob::new(MemoryBlob::with_defect(Defect::KeysFoldCase).with_hex_layout());
        kv.put("Key", b"upper".to_vec(), None).await.unwrap();
        kv.put("key", b"lower".to_vec(), None).await.unwrap();
        assert_eq!(kv.get("Key").await.unwrap().as_deref(), Some(&b"upper"[..]));
        assert_eq!(kv.get("key").await.unwrap().as_deref(), Some(&b"lower"[..]));
        // The control: with the raw layout the same store aliases them.
        let raw = KvOnBlob::new(MemoryBlob::with_defect(Defect::KeysFoldCase));
        raw.put("Key", b"upper".to_vec(), None).await.unwrap();
        raw.put("key", b"lower".to_vec(), None).await.unwrap();
        assert_eq!(
            raw.get("Key").await.unwrap().as_deref(),
            Some(&b"lower"[..])
        );
    }

    #[tokio::test]
    async fn listing_gives_the_keys_back_in_order_by_prefix_and_page() {
        let kv = hex_store();
        let mut keys = awkward();
        keys.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
        keys.dedup();
        for k in &keys {
            kv.put(k, k.as_bytes().to_vec(), None).await.unwrap();
        }
        // Everything, in pages of 7.
        let mut got = Vec::new();
        let mut after: Option<String> = None;
        loop {
            let page = kv.ordered_scan("", after.as_deref(), 7).await.unwrap();
            got.extend(page.keys.iter().cloned());
            match page.next {
                Some(n) => after = Some(n),
                None => break,
            }
        }
        assert_eq!(got, keys);
        // By prefix.
        let want: Vec<String> = keys
            .iter()
            .filter(|k| k.starts_with("xxxx"))
            .cloned()
            .collect();
        assert!(want.len() > 3, "control: the prefix matches several keys");
        let page = kv.prefix_list("xxxx", None, 1000).await.unwrap();
        assert_eq!(page.keys, want);
        for k in &keys {
            assert_eq!(
                kv.get(k).await.unwrap().as_deref(),
                Some(k.as_bytes()),
                "{k:?}"
            );
        }
    }

    #[tokio::test]
    async fn the_raw_layout_is_unchanged() {
        let kv = KvOnBlob::new(MemoryBlob::new());
        kv.put("a/b", b"v".to_vec(), None).await.unwrap();
        assert!(kv.blob().read("kv/a/b").await.unwrap().is_some());
        assert_eq!(kv.migrate_raw_layout().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn raw_records_move_to_the_hex_layout_once_and_never_overwrite() {
        let blob = MemoryBlob::new().with_hex_layout();
        for (k, v) in [("a", "1"), ("b/c", "2"), ("Key", "3"), ("newer", "old")] {
            blob.write(&format!("kv/{k}"), encode(v.as_bytes(), None))
                .await
                .unwrap();
        }
        blob.write("kv/gone", encode(b"x", Some(Duration::from_millis(1))))
            .await
            .unwrap();
        let kv = KvOnBlob::new(blob);
        // A value already written under the new layout is newer than the raw copy.
        kv.put("newer", b"new".to_vec(), None).await.unwrap();
        std::thread::sleep(Duration::from_millis(5));
        assert_eq!(
            kv.migrate_raw_layout().await.unwrap(),
            4,
            "the expired record is dropped"
        );
        assert_eq!(kv.get("a").await.unwrap().as_deref(), Some(&b"1"[..]));
        assert_eq!(kv.get("b/c").await.unwrap().as_deref(), Some(&b"2"[..]));
        assert_eq!(kv.get("Key").await.unwrap().as_deref(), Some(&b"3"[..]));
        assert_eq!(kv.get("newer").await.unwrap().as_deref(), Some(&b"new"[..]));
        assert_eq!(kv.get("gone").await.unwrap(), None);
        assert!(
            kv.blob()
                .list("kv/", None, 10)
                .await
                .unwrap()
                .entries
                .is_empty()
        );
        assert_eq!(
            kv.migrate_raw_layout().await.unwrap(),
            0,
            "repeating it moves nothing"
        );
    }
}
