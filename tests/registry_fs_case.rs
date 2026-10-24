// SPDX-License-Identifier: MIT OR Apache-2.0
//! The registry's `fs` stores each key under its bytes in lower-case hex, so it opens on every
//! filesystem (NTFS and APFS ignore case by default) and keys that differ only by case, keys with
//! separators, and a key that is another key's prefix all stay apart. A directory written by
//! 0.9.41 or earlier (raw `kv/<key>` files) is migrated when it is opened.
#![cfg(feature = "registry")]

use persistant::blob::BlobBackend;
use persistant::capability::{CapabilityId, choose::Durability, provider::ProviderNeeds};
use persistant::kv::KvBackend;
use persistant::opendal_driver::OpendalBlob;
use persistant::registry::{self, Endpoint};

fn needs() -> ProviderNeeds {
    ProviderNeeds::new(
        ["kv.get", "kv.put", "kv.prefix_list"]
            .iter()
            .map(|s| CapabilityId::new(*s).expect("a literal id"))
            .collect(),
        Durability::Process,
    )
}

#[tokio::test]
async fn fs_opens_everywhere_and_keys_that_differ_only_by_case_never_share_a_value() {
    let dir = tempfile::tempdir().unwrap();
    let h = registry::open_named("fs", &Endpoint::path(dir.path().join("s")), &needs())
        .await
        .expect("fs opens on every filesystem now");
    let kv = h.kv();
    for (k, v) in [
        ("Key", "upper"),
        ("key", "lower"),
        ("a", "A"),
        ("a/b", "AB"),
    ] {
        kv.put(k, v.as_bytes().to_vec(), None).await.unwrap();
    }
    assert_eq!(kv.get("Key").await.unwrap().as_deref(), Some(&b"upper"[..]));
    assert_eq!(kv.get("key").await.unwrap().as_deref(), Some(&b"lower"[..]));
    assert_eq!(kv.get("a/b").await.unwrap().as_deref(), Some(&b"AB"[..]));
    let keys = kv.prefix_list("", None, 100).await.unwrap().keys;
    assert_eq!(keys, ["Key", "a", "a/b", "key"]);
}

#[tokio::test]
async fn a_directory_written_before_the_encoding_is_migrated_at_open() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("s");
    let mut scratch = root.clone().into_os_string();
    scratch.push(".persistant-atomic");
    let scratch = std::path::PathBuf::from(scratch);
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&scratch).unwrap();
    // The 0.9.41 layout: `kv/<key>`, one version byte, eight bytes of expiry, the value.
    let old = OpendalBlob::fs(&root, Some(&scratch)).unwrap();
    let envelope = |v: &[u8]| [&[1u8][..], &[0u8; 8], v].concat();
    old.write("kv/settings/theme", envelope(b"dark"))
        .await
        .unwrap();
    let h = registry::open_named("fs", &Endpoint::path(&root), &needs())
        .await
        .unwrap();
    assert_eq!(
        h.kv().get("settings/theme").await.unwrap().as_deref(),
        Some(&b"dark"[..])
    );
    assert!(old.read("kv/settings/theme").await.unwrap().is_none());
    // Control: opening again changes nothing.
    drop(h);
    let h = registry::open_named("fs", &Endpoint::path(&root), &needs())
        .await
        .unwrap();
    assert_eq!(
        h.kv().get("settings/theme").await.unwrap().as_deref(),
        Some(&b"dark"[..])
    );
}

#[tokio::test]
async fn the_other_ids_are_unaffected() {
    // `memory` has no directory and keeps keys as they are.
    let needs = ProviderNeeds::new(
        ["kv.get", "kv.put"]
            .iter()
            .map(|s| CapabilityId::new(*s).expect("a literal id"))
            .collect(),
        Durability::Volatile,
    );
    let h = registry::open_named("memory", &Endpoint::None, &needs)
        .await
        .expect("memory opens everywhere");
    h.kv().put("Key", b"a".to_vec(), None).await.unwrap();
    h.kv().put("key", b"b".to_vec(), None).await.unwrap();
    assert_eq!(h.kv().get("Key").await.unwrap().as_deref(), Some(&b"a"[..]));
}
