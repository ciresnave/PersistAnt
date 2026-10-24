// SPDX-License-Identifier: MIT OR Apache-2.0
//! `fs` with `atomic_write_dir`: exclusive create is a hard link of a finished scratch file, so of
//! many concurrent creators exactly one wins and the value is whole the moment the key exists.
//!
//! The trial runs on real threads (a multi-thread runtime), and has a negative control: the old
//! check-then-write create, with every contender made to check before any writes, lets all of them
//! win. A test that cannot fail that way proves nothing about the hard link.

use persistant_blob::BlobBackend;
use persistant_opendal::OpendalBlob;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::Barrier;

const CONTENDERS: usize = 16;
const ROUNDS: usize = 30;
const LEN: usize = 256 * 1024;

fn payload(i: usize) -> Vec<u8> {
    vec![i as u8; LEN]
}

fn scratch_files(scratch: &Path) -> usize {
    std::fs::read_dir(scratch).unwrap().count()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn exactly_one_of_many_concurrent_creators_wins_and_the_value_is_whole() {
    let root = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let b = Arc::new(OpendalBlob::fs(root.path(), Some(scratch.path())).unwrap());
    for round in 0..ROUNDS {
        let key = format!("kv/round-{round}");
        let start = Arc::new(Barrier::new(CONTENDERS));
        let tasks: Vec<_> = (0..CONTENDERS)
            .map(|i| {
                let (b, key, start) = (b.clone(), key.clone(), start.clone());
                tokio::spawn(async move {
                    start.wait().await;
                    b.create_exclusive(&key, payload(i)).await
                })
            })
            .collect();
        let mut winners = Vec::new();
        for (i, t) in tasks.into_iter().enumerate() {
            if t.await.unwrap().unwrap() {
                winners.push(i);
            }
        }
        assert_eq!(winners.len(), 1, "round {round}: winners {winners:?}");
        let stored = b.read(&key).await.unwrap().unwrap();
        assert!(
            stored == payload(winners[0]),
            "round {round}: the stored value is not the winner's, whole ({} bytes)",
            stored.len()
        );
    }
    assert_eq!(
        scratch_files(scratch.path()),
        0,
        "no scratch file is left behind"
    );
}

/// The control: what OpenDAL's own create under a scratch directory did (check, then write), with a
/// barrier so every contender checks before any writes. The same trial must see many winners.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn control_check_then_write_lets_every_contender_win() {
    let root = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let b = Arc::new(OpendalBlob::fs(root.path(), Some(scratch.path())).unwrap());
    let key = "kv/control";
    let checked = Arc::new(Barrier::new(CONTENDERS));
    let tasks: Vec<_> = (0..CONTENDERS)
        .map(|i| {
            let (b, checked) = (b.clone(), checked.clone());
            tokio::spawn(async move {
                let absent = b.stat(key).await.unwrap().is_none();
                checked.wait().await;
                if absent {
                    b.write(key, payload(i)).await.unwrap();
                }
                absent
            })
        })
        .collect();
    let mut winners = 0;
    for t in tasks {
        winners += usize::from(t.await.unwrap());
    }
    assert_eq!(
        winners, CONTENDERS,
        "control: check-then-write has no single winner"
    );
}

#[tokio::test]
async fn an_existing_key_is_left_alone() {
    let root = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let b = OpendalBlob::fs(root.path(), Some(scratch.path())).unwrap();
    assert!(
        b.create_exclusive("a/b/c", b"first".to_vec())
            .await
            .unwrap()
    );
    assert!(
        !b.create_exclusive("a/b/c", b"second".to_vec())
            .await
            .unwrap()
    );
    assert_eq!(b.read("a/b/c").await.unwrap().unwrap(), b"first");
    // An ordinary write still replaces it (create_exclusive is only about the create).
    b.write("a/b/c", b"third".to_vec()).await.unwrap();
    assert_eq!(b.read("a/b/c").await.unwrap().unwrap(), b"third");
    assert_eq!(scratch_files(scratch.path()), 0);
}

#[tokio::test]
async fn a_key_outside_the_root_is_refused() {
    let root = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let b = OpendalBlob::fs(root.path(), Some(scratch.path())).unwrap();
    for key in ["../escape", "a/../../escape", ""] {
        assert!(
            b.create_exclusive(key, b"x".to_vec()).await.is_err(),
            "{key:?}"
        );
    }
    assert!(!root.path().parent().unwrap().join("escape").exists());
    // Control: a plain nested key is fine.
    assert!(b.create_exclusive("a/b", b"x".to_vec()).await.unwrap());
}

#[tokio::test]
async fn without_a_scratch_directory_it_is_still_opendals_exclusive_create() {
    let root = tempfile::tempdir().unwrap();
    let b = OpendalBlob::fs(root.path(), None).unwrap();
    assert!(b.create_exclusive("k", b"one".to_vec()).await.unwrap());
    assert!(!b.create_exclusive("k", b"two".to_vec()).await.unwrap());
}
