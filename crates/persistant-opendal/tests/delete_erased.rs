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
