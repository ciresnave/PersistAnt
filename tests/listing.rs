// SPDX-License-Identifier: MIT OR Apache-2.0
//! `stat`, bounded `list`, and `create_if_absent`, on the `fs` and `memory` backends.

use std::num::NonZeroUsize;
use std::sync::Arc;

use persistant::{Config, Error, Need, Needs, Store};

fn needs() -> Needs {
    Needs::new()
        .with(Need::Read)
        .with(Need::Write)
        .with(Need::Stat)
        .with(Need::List)
        .with(Need::CreateIfAbsent)
}

fn n(v: usize) -> NonZeroUsize {
    NonZeroUsize::new(v).unwrap()
}

/// A store on `memory`, or on `fs` in a fresh directory (kept alive by the returned guard).
async fn open(fs: bool) -> (Store, Option<tempfile::TempDir>) {
    if fs {
        let dir = tempfile::tempdir().unwrap();
        let cfg = Config::Fs {
            root: dir.path().to_path_buf(),
            atomic_write_dir: None,
        };
        (Store::open(cfg, needs()).await.unwrap(), Some(dir))
    } else {
        (Store::open(Config::Memory, needs()).await.unwrap(), None)
    }
}

async fn put(s: &Store, key: &str, bytes: &[u8]) {
    s.create_if_absent(key, bytes.to_vec()).await.unwrap();
}

fn keys(page: &persistant::Page) -> Vec<&str> {
    page.entries.iter().map(|e| e.key.as_str()).collect()
}

// ---- stat

#[tokio::test]
async fn stat_reports_length_of_a_present_key() {
    for fs in [false, true] {
        let (s, _g) = open(fs).await;
        put(&s, "a/b.bin", b"hello").await;
        let e = s.stat("a/b.bin").await.unwrap();
        assert_eq!(e.key, "a/b.bin", "fs={fs}");
        assert_eq!(e.len, 5, "fs={fs}");
        if fs {
            assert!(e.modified.is_some(), "fs reports a modification time");
        }
    }
}

#[tokio::test]
async fn stat_of_absent_key_and_of_a_directory_is_not_found() {
    for fs in [false, true] {
        let (s, _g) = open(fs).await;
        put(&s, "a/b.bin", b"x").await;
        assert!(
            matches!(s.stat("nope").await, Err(Error::NotFound(k)) if k == "nope"),
            "fs={fs}"
        );
        assert!(
            matches!(s.stat("a").await, Err(Error::NotFound(_))),
            "a directory is not a key (fs={fs})"
        );
        assert!(
            matches!(s.stat("a/").await, Err(Error::NotFound(_))),
            "fs={fs}"
        );
    }
}

#[tokio::test]
async fn stat_and_list_need_their_declarations() {
    let s = Store::open(
        Config::Memory,
        Needs::new().with(Need::Read).with(Need::Write),
    )
    .await
    .unwrap();
    assert!(matches!(
        s.stat("k").await,
        Err(Error::NotDeclared(Need::Stat))
    ));
    assert!(matches!(
        s.list("", None, n(1)).await,
        Err(Error::NotDeclared(Need::List))
    ));
    assert!(matches!(
        s.create_if_absent("k", vec![]).await,
        Err(Error::NotDeclared(Need::CreateIfAbsent))
    ));
}

// ---- list

#[tokio::test]
async fn list_is_sorted_nested_and_prefix_is_a_string_prefix() {
    for fs in [false, true] {
        let (s, _g) = open(fs).await;
        for k in [
            "img/b.png",
            "img/a.png",
            "img/ab/c.png",
            "img/a/deep/er.png",
            "doc/x.txt",
            "top.txt",
        ] {
            put(&s, k, k.as_bytes()).await;
        }
        let all = s.list("", None, n(100)).await.unwrap();
        assert_eq!(
            keys(&all),
            [
                "doc/x.txt",
                "img/a.png",
                "img/a/deep/er.png",
                "img/ab/c.png",
                "img/b.png",
                "top.txt"
            ],
            "fs={fs}"
        );
        assert_eq!(all.next, None);
        assert!(all.entries.iter().all(|e| e.len == e.key.len() as u64));

        // `img/a` is a string prefix: not a directory, matches a file, a dir and a nested file.
        let p = s.list("img/a", None, n(100)).await.unwrap();
        assert_eq!(
            keys(&p),
            ["img/a.png", "img/a/deep/er.png", "img/ab/c.png"],
            "fs={fs}"
        );
        // a trailing slash narrows to the directory
        let d = s.list("img/a/", None, n(100)).await.unwrap();
        assert_eq!(keys(&d), ["img/a/deep/er.png"], "fs={fs}");
        // prefix with no match, and under a directory that does not exist
        assert!(s.list("zzz", None, n(10)).await.unwrap().entries.is_empty());
        assert!(
            s.list("nodir/x", None, n(10))
                .await
                .unwrap()
                .entries
                .is_empty()
        );
    }
}

#[tokio::test]
async fn list_pages_cover_every_key_once_and_bound_each_page() {
    for fs in [false, true] {
        let (s, _g) = open(fs).await;
        for i in 0..5 {
            put(&s, &format!("k/{i}"), b"v").await;
        }
        let mut seen = Vec::new();
        let mut after: Option<String> = None;
        let mut pages = 0;
        loop {
            let page = s.list("k/", after.as_deref(), n(2)).await.unwrap();
            assert!(page.entries.len() <= 2, "fs={fs}");
            seen.extend(page.entries.iter().map(|e| e.key.clone()));
            pages += 1;
            match page.next {
                Some(c) => {
                    assert_eq!(Some(&c), seen.last(), "cursor is the last key returned");
                    after = Some(c);
                }
                None => break,
            }
        }
        assert_eq!(seen, ["k/0", "k/1", "k/2", "k/3", "k/4"], "fs={fs}");
        assert_eq!(pages, 3, "fs={fs}");
    }
}

#[tokio::test]
async fn list_with_exactly_limit_keys_has_no_next_page() {
    for fs in [false, true] {
        let (s, _g) = open(fs).await;
        put(&s, "a", b"1").await;
        put(&s, "b", b"2").await;
        let exact = s.list("", None, n(2)).await.unwrap();
        assert_eq!(keys(&exact), ["a", "b"]);
        assert_eq!(exact.next, None, "fs={fs}: nothing follows, so no cursor");
        let one_short = s.list("", None, n(1)).await.unwrap();
        assert_eq!(keys(&one_short), ["a"]);
        assert_eq!(one_short.next.as_deref(), Some("a"), "fs={fs}");
    }
}

#[tokio::test]
async fn list_after_is_exclusive_and_works_for_a_cursor_that_no_longer_exists() {
    for fs in [false, true] {
        let (s, _g) = open(fs).await;
        for k in ["a", "c", "e"] {
            put(&s, k, b"v").await;
        }
        let p = s.list("", Some("c"), n(10)).await.unwrap();
        assert_eq!(keys(&p), ["e"], "fs={fs}: `c` itself is excluded");
        let p = s.list("", Some("b"), n(10)).await.unwrap();
        assert_eq!(keys(&p), ["c", "e"], "fs={fs}: cursor key need not exist");
    }
}

#[tokio::test]
async fn list_is_refused_at_open_on_a_backend_without_it() {
    let err = Store::open(
        Config::Masked {
            inner: Box::new(Config::Memory),
            without: vec![Need::List, Need::CreateIfAbsent],
        },
        needs(),
    )
    .await
    .unwrap_err();
    match err {
        Error::Refused { missing, .. } => {
            let got: Vec<Need> = missing.iter().map(|m| m.need).collect();
            assert_eq!(got, [Need::List, Need::CreateIfAbsent]);
        }
        other => panic!("expected Refused, got {other:?}"),
    }
}

// ---- create_if_absent

#[tokio::test]
async fn create_if_absent_loses_to_an_existing_value_and_leaves_it_alone() {
    for fs in [false, true] {
        let (s, _g) = open(fs).await;
        s.create_if_absent("lock", b"first".to_vec()).await.unwrap();
        let err = s
            .create_if_absent("lock", b"second".to_vec())
            .await
            .unwrap_err();
        assert!(
            matches!(&err, Error::AlreadyExists(k) if k == "lock"),
            "fs={fs}: {err:?}"
        );
        assert_eq!(s.read("lock").await.unwrap(), b"first", "fs={fs}");
    }
}

#[tokio::test]
async fn create_if_absent_has_exactly_one_winner_under_contention() {
    for fs in [false, true] {
        let (s, _g) = open(fs).await;
        let s = Arc::new(s);
        let mut tasks = Vec::new();
        for i in 0..16u8 {
            let s = s.clone();
            tasks.push(tokio::spawn(async move {
                s.create_if_absent("race", vec![i]).await
            }));
        }
        let mut wins = 0;
        for t in tasks {
            match t.await.unwrap() {
                Ok(()) => wins += 1,
                Err(Error::AlreadyExists(_)) => {}
                Err(e) => panic!("fs={fs}: unexpected {e:?}"),
            }
        }
        assert_eq!(wins, 1, "fs={fs}");
    }
}

#[tokio::test]
async fn create_if_absent_is_refused_on_fs_with_atomic_write_dir() {
    let root = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let err = Store::open(
        Config::Fs {
            root: root.path().to_path_buf(),
            atomic_write_dir: Some(scratch.path().to_path_buf()),
        },
        Needs::new().with(Need::Write).with(Need::CreateIfAbsent),
    )
    .await
    .unwrap_err();
    match err {
        Error::Refused { backend, missing } => {
            assert_eq!(backend, "fs");
            assert_eq!(missing.len(), 1);
            assert_eq!(missing[0].need, Need::CreateIfAbsent);
            assert!(missing[0].detail.contains("atomic_write_dir"));
        }
        other => panic!("expected Refused, got {other:?}"),
    }
    // control: the same config is fine when CreateIfAbsent is not declared
    Store::open(
        Config::Fs {
            root: root.path().to_path_buf(),
            atomic_write_dir: Some(scratch.path().to_path_buf()),
        },
        Needs::new().with(Need::Write),
    )
    .await
    .unwrap();
}

// ---- blocking facade

#[test]
fn blocking_facade_has_the_same_three_operations() {
    let s = persistant::blocking::Store::open(Config::Memory, needs()).unwrap();
    s.create_if_absent("p/a", b"abc".to_vec()).unwrap();
    assert!(matches!(
        s.create_if_absent("p/a", vec![]),
        Err(Error::AlreadyExists(_))
    ));
    assert_eq!(s.stat("p/a").unwrap().len, 3);
    let page = s.list("p/", None, n(5)).unwrap();
    assert_eq!(keys(&page), ["p/a"]);
}
