// SPDX-License-Identifier: MIT OR Apache-2.0
//! `KvOnBlob` over a real `fs` directory stores each key under its bytes in lower-case hex, so
//! whatever the filesystem does with case, separators, dots or reserved names, every key is its own
//! value, and listing still gives the keys back in order. A directory written with raw names (0.9.41
//! and earlier) is moved to the hex layout by `migrate_raw_layout`.

use persistant_blob::BlobBackend;
use persistant_core::Config;
use persistant_kv::conformance::{Certified, certify};
use persistant_kv::{KvBackend, KvOnBlob};
use persistant_opendal::OpendalBlob;

fn summary(rows: &[Certified]) -> String {
    rows.iter()
        .map(|c| {
            format!(
                "{}={}",
                c.leaf.id(),
                if c.row.enabled { "on" } else { "off" }
            )
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Keys that a filesystem treats badly if used as file names: case, separators, dots, reserved
/// device names, a key that is another key's directory, and one longer than a file name may be.
fn awkward_keys() -> Vec<String> {
    let mut v: Vec<String> = [
        "",
        "Key",
        "key",
        "KEY",
        "a",
        "a/b",
        "a/b/c",
        "..",
        "../up",
        "CON",
        "con",
        "nul",
        "a:b",
        "trailing.",
        "trailing ",
        "caf\u{e9}",
        "CAF\u{c9}",
        "100%",
        "k\0ey",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    v.push("x".repeat(300));
    v.push(format!("{}y", "x".repeat(300)));
    v.push("X".repeat(300));
    v
}

fn fs_kv(root: &std::path::Path, scratch: &std::path::Path) -> KvOnBlob<OpendalBlob> {
    KvOnBlob::new(OpendalBlob::fs(root, Some(scratch)).unwrap())
}

#[tokio::test]
async fn every_awkward_key_is_its_own_value_and_lists_in_order() {
    // On NTFS or APFS (case-insensitive) as much as on ext4: the keys are stored as hex.
    let root = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let kv = fs_kv(root.path(), scratch.path());
    let mut keys = awkward_keys();
    for k in &keys {
        kv.put(k, k.as_bytes().to_vec(), None).await.unwrap();
    }
    for k in &keys {
        assert_eq!(
            kv.get(k).await.unwrap().as_deref(),
            Some(k.as_bytes()),
            "{k:?}"
        );
        assert!(
            !kv.put_if_absent(k, b"other".to_vec(), None).await.unwrap(),
            "{k:?}"
        );
    }
    keys.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
    let mut got = Vec::new();
    let mut after: Option<String> = None;
    loop {
        let page = kv.ordered_scan("", after.as_deref(), 5).await.unwrap();
        got.extend(page.keys.clone());
        match page.next {
            Some(n) => after = Some(n),
            None => break,
        }
    }
    assert_eq!(got, keys, "ordered, none missing, none aliased");
    let page = kv.prefix_list("a", None, 100).await.unwrap();
    assert_eq!(page.keys, ["a", "a/b", "a/b/c", "a:b"]);
    kv.delete("a/b").await.unwrap();
    assert_eq!(kv.get("a/b").await.unwrap(), None);
    assert_eq!(kv.get("a").await.unwrap().as_deref(), Some(&b"a"[..]));
    // Nothing escaped the root, and the directory holds only the hex layout.
    let top: Vec<_> = std::fs::read_dir(root.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
        .collect();
    assert_eq!(top, ["kvx"]);
}

#[tokio::test]
async fn the_kv_leaves_hold_whatever_the_filesystem_does_with_case() {
    // kv.get checks keys that differ only in case; it is on everywhere now.
    let root = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let kv = fs_kv(root.path(), scratch.path());
    let mut facts = kv.blob().config().to_string();
    facts.push(';');
    facts.push_str(&kv.config().to_string());
    let rows = certify(&kv, &Config::parse(&facts).unwrap(), &Config::new()).await;
    for id in ["kv.get", "kv.put", "kv.prefix_list", "kv.ordered_scan"] {
        let c = rows.iter().find(|c| c.leaf.id() == id).unwrap();
        assert!(c.row.enabled, "{id}: {}", summary(&rows));
    }
}

#[tokio::test]
async fn a_directory_written_with_raw_names_is_migrated() {
    let root = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    // What 0.9.41 and earlier wrote: the envelope under `kv/<key>` (one version byte, eight bytes
    // of expiry, then the value).
    let raw = OpendalBlob::fs(root.path(), Some(scratch.path())).unwrap();
    let envelope = |v: &[u8]| [&[1u8][..], &[0u8; 8], v].concat();
    raw.write("kv/old", envelope(b"one")).await.unwrap();
    raw.write("kv/dir/inner", envelope(b"two")).await.unwrap();
    let kv = fs_kv(root.path(), scratch.path());
    assert_eq!(
        kv.get("old").await.unwrap(),
        None,
        "not visible before the move"
    );
    assert_eq!(kv.migrate_raw_layout().await.unwrap(), 2);
    assert_eq!(kv.get("old").await.unwrap().as_deref(), Some(&b"one"[..]));
    assert_eq!(
        kv.get("dir/inner").await.unwrap().as_deref(),
        Some(&b"two"[..])
    );
    assert!(
        raw.read("kv/old").await.unwrap().is_none(),
        "the raw copy is gone"
    );
    assert_eq!(kv.migrate_raw_layout().await.unwrap(), 0);
}
