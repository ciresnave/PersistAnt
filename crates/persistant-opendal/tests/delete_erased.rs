// SPDX-License-Identifier: MIT OR Apache-2.0
//! `blob.delete_erased` for the `fs` driver, read back through a second hard link to the same
//! file: unlinking a key leaves the link's bytes alone, so a plain `delete` keeps the canary
//! readable there (the known-bad control) and `delete_erased` must not.
//!
//! **Not covered:** copies a journaling or copy-on-write file system, an SSD, a snapshot or a
//! backup keeps; the file's freed blocks (read through a second link, the same inode, not the raw
//! device).

use persistant_blob::{BlobBackend, BlobError};
use persistant_opendal::OpendalBlob;

const CANARY: &[u8] = b"CANARY-9e41d0b7-not-a-secret";

fn setup() -> (tempfile::TempDir, OpendalBlob, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    let b = OpendalBlob::fs(&root, None).unwrap();
    (dir, b, root)
}

fn holds_canary(path: &std::path::Path) -> bool {
    std::fs::read(path)
        .unwrap()
        .windows(CANARY.len())
        .any(|w| w == CANARY)
}

#[tokio::test]
async fn plain_delete_leaves_the_bytes_readable_through_a_second_link() {
    let (dir, b, root) = setup();
    b.write("k", CANARY.to_vec()).await.unwrap();
    let alias = dir.path().join("alias");
    std::fs::hard_link(root.join("k"), &alias).unwrap();
    b.delete("k").await.unwrap();
    assert_eq!(b.read("k").await.unwrap(), None);
    assert!(holds_canary(&alias), "the control: delete keeps the bytes");
}

#[tokio::test]
async fn delete_erased_overwrites_before_unlinking() {
    let (dir, b, root) = setup();
    // Larger than the overwrite buffer, canary in the middle and at the end.
    let mut v = vec![b'.'; 20_000];
    v.extend_from_slice(CANARY);
    v.extend_from_slice(&[b'.'; 10]);
    b.write("d/k", v.clone()).await.unwrap();
    let alias = dir.path().join("alias");
    std::fs::hard_link(root.join("d/k"), &alias).unwrap();
    assert!(holds_canary(&alias));
    b.delete_erased("d/k").await.unwrap();
    assert_eq!(b.read("d/k").await.unwrap(), None);
    assert!(!holds_canary(&alias), "the canary survived the erase");
    assert_eq!(std::fs::read(&alias).unwrap(), vec![0u8; v.len()]);
}

#[tokio::test]
async fn delete_erased_of_an_absent_key_is_ok() {
    let (_dir, b, _root) = setup();
    b.delete_erased("nope").await.unwrap();
}

#[tokio::test]
async fn the_memory_service_says_unsupported() {
    let b = OpendalBlob::memory().unwrap();
    b.write("k", CANARY.to_vec()).await.unwrap();
    assert!(matches!(
        b.delete_erased("k").await,
        Err(BlobError::Unsupported(_))
    ));
    // Nothing was removed by the refusal.
    assert!(b.read("k").await.unwrap().is_some());
}

fn only_file(dir: &std::path::Path) -> std::path::PathBuf {
    let mut found = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(d).unwrap() {
            let e = e.unwrap();
            if e.file_type().unwrap().is_dir() {
                stack.push(e.path());
            } else {
                found.push(e.path());
            }
        }
    }
    assert_eq!(found.len(), 1, "{found:?}");
    found.remove(0)
}

/// `kv.delete_erased` over `fs`: the record file (value inside an envelope) is overwritten.
#[tokio::test]
async fn kv_delete_erased_overwrites_the_record_file() {
    use persistant_kv::{KvBackend, KvOnBlob};
    let (dir, b, root) = setup();
    let kv = KvOnBlob::with_in_place_put(b);
    kv.put("user/1", CANARY.to_vec(), None).await.unwrap();
    let alias = dir.path().join("alias");
    std::fs::hard_link(only_file(&root), &alias).unwrap();
    assert!(holds_canary(&alias));
    kv.delete_erased("user/1").await.unwrap();
    assert_eq!(kv.get("user/1").await.unwrap(), None);
    assert!(!holds_canary(&alias), "the value survived the erase");
    kv.delete_erased("absent").await.unwrap();
}

/// Control: plain `kv.delete` leaves the value readable through the second link.
#[tokio::test]
async fn kv_plain_delete_keeps_the_record_bytes() {
    use persistant_kv::{KvBackend, KvOnBlob};
    let (dir, b, root) = setup();
    let kv = KvOnBlob::with_in_place_put(b);
    kv.put("user/1", CANARY.to_vec(), None).await.unwrap();
    let alias = dir.path().join("alias");
    std::fs::hard_link(only_file(&root), &alias).unwrap();
    kv.delete("user/1").await.unwrap();
    assert!(holds_canary(&alias));
}
