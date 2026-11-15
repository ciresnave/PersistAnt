// SPDX-License-Identifier: MIT OR Apache-2.0
//! `TextIndex` over a real SQLite file through `KvOnRelational`: the same operations as the memory
//! test, a replace that drops a term, a removal, and concurrent writers whose lost races retry.
#![cfg(all(feature = "text-index", feature = "registry-sqlite"))]

use persistant::kv::text::{Match, TextIndex};
use persistant::kv::{KvBackend, KvOnRelational};
use persistant::sqlx_driver::SqliteBackend;
use std::sync::Arc;

fn dir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("persistant-text-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[tokio::test]
async fn operations_on_sqlite() {
    let d = dir("ops");
    let kv = KvOnRelational::open(SqliteBackend::open(d.join("k.db")).await.unwrap())
        .await
        .unwrap();
    let ix = TextIndex::new(&kv);
    ix.put("i", "a/1", &[("rust".into(), 3), ("kv".into(), 1)])
        .await
        .unwrap();
    ix.put("i", "b/1", &[("rust".into(), 1), ("sql".into(), 2)])
        .await
        .unwrap();
    ix.put("i", "c/1", &[("sql".into(), 5)]).await.unwrap();
    let s = ix.stats("i").await.unwrap();
    assert_eq!((s.docs, s.total_len), (3, 12));
    let q = |w: &[&str]| w.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let hits = ix
        .search("i", &q(&["rust", "sql"]), Match::Any, 10)
        .await
        .unwrap();
    let got: Vec<(&str, u64)> = hits.iter().map(|h| (h.doc_id.as_str(), h.score)).collect();
    assert_eq!(got, [("c/1", 5), ("a/1", 3), ("b/1", 3)]);
    let all = ix
        .search("i", &q(&["rust", "sql"]), Match::All, 10)
        .await
        .unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].doc_id, "b/1");

    // A replace that drops a term leaves no posting for it and moves the totals.
    ix.put("i", "a/1", &[("rust".into(), 2)]).await.unwrap();
    assert!(
        ix.postings("i", "kv", None, 10)
            .await
            .unwrap()
            .entries
            .is_empty()
    );
    assert_eq!(ix.stats("i").await.unwrap().total_len, 10);
    assert_eq!(ix.remove_many("i", &["b/1", "nope"]).await.unwrap(), 1);
    let s = ix.stats("i").await.unwrap();
    assert_eq!((s.docs, s.total_len), (2, 7));
    assert_eq!(ix.docs("i", None, 10).await.unwrap().ids, ["a/1", "c/1"]);
    ix.remove_many("i", &["a/1", "c/1"]).await.unwrap();
    assert_eq!(ix.stats("i").await.unwrap().docs, 0);
    // An emptied index holds no keys of its own.
    assert!(
        kv.ordered_scan("t", None, 10)
            .await
            .unwrap()
            .keys
            .is_empty()
    );
    drop(kv);
    let _ = std::fs::remove_dir_all(&d);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_writers_on_sqlite_keep_the_totals_exact() {
    let d = dir("conc");
    let kv = Arc::new(
        KvOnRelational::open(SqliteBackend::open(d.join("k.db")).await.unwrap())
            .await
            .unwrap(),
    );
    let tasks: Vec<_> = (0..8)
        .map(|i| {
            let kv = kv.clone();
            tokio::spawn(async move {
                TextIndex::new(&*kv)
                    .put("i", &format!("d{i}"), &[("alpha".into(), 1 + i as u32)])
                    .await
                    .unwrap();
            })
        })
        .collect();
    for t in tasks {
        t.await.unwrap();
    }
    let ix = TextIndex::new(&*kv);
    let s = ix.stats("i").await.unwrap();
    assert_eq!((s.docs, s.total_len), (8, (1..=8).sum()));
    assert_eq!(
        ix.postings("i", "alpha", None, 100)
            .await
            .unwrap()
            .entries
            .len(),
        8
    );
    drop(kv);
    let _ = std::fs::remove_dir_all(&d);
}
