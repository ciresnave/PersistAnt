// SPDX-License-Identifier: MIT OR Apache-2.0
//! `TextIndex::remove_many_erased`: the documents' terms (the postings' keys) are in none of the
//! database's files afterwards, read back as raw bytes, on SQLite and on `sqlite-full` (WAL). A plain
//! `remove_many` is the control: it leaves the canary term in the files. The rest of the index is
//! intact, the totals are right, a backend that cannot erase in a batch (memory) refuses before
//! anything changes, and an unknown document is ignored.
#![cfg(all(feature = "text-index", feature = "registry-sqlite"))]

use persistant::capability::CapabilityId;
use persistant::capability::choose::Durability;
use persistant::capability::provider::ProviderNeeds;
use persistant::kv::text::{Match, TextIndex};
use persistant::kv::{KvError, MemoryKv};
use persistant::registry::{self, Endpoint};
use std::path::{Path, PathBuf};

const CANARY: &str = "zzcanaryterm7f3a91";

fn needs() -> ProviderNeeds {
    let id = |s: &str| CapabilityId::new(s).unwrap();
    ProviderNeeds::new(
        vec![
            id("kv.get"),
            id("kv.put"),
            id("kv.batch_atomic.rollback"),
            id("kv.ordered_scan_values"),
        ],
        Durability::Volatile,
    )
}

fn dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("persistant-texterase-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn holds(d: &Path) -> bool {
    std::fs::read_dir(d).unwrap().any(|e| {
        std::fs::read(e.unwrap().path())
            .unwrap()
            .windows(CANARY.len())
            .any(|w| w == CANARY.as_bytes())
    })
}

fn terms(words: &[(&str, u32)]) -> Vec<(String, u32)> {
    words.iter().map(|(w, n)| (w.to_string(), *n)).collect()
}

/// Twenty filler documents so the canary's page holds other cells, then the two that matter.
async fn fill<K: persistant::kv::KvBackend>(ix: &TextIndex<'_, K>) {
    for i in 0..20 {
        ix.put(
            "i",
            &format!("f{i:02}"),
            &terms(&[("filler", 1), (&format!("word{i}"), 2)]),
        )
        .await
        .unwrap();
    }
    ix.put("i", "d1", &terms(&[(CANARY, 3), ("shared", 1)]))
        .await
        .unwrap();
    ix.put("i", "d2", &terms(&[("shared", 2), ("keep", 1)]))
        .await
        .unwrap();
}

#[tokio::test]
async fn an_erased_removal_leaves_no_term_in_the_files_and_the_rest_intact() {
    for name in ["sqlite", "sqlite-full"] {
        let (plain_dir, erased_dir) = (dir(&format!("p-{name}")), dir(&format!("e-{name}")));
        // The control: a plain removal leaves the canary term in the files.
        let h = registry::open_named(name, &Endpoint::path(plain_dir.join("k.db")), &needs())
            .await
            .unwrap();
        let ix = TextIndex::new(h.kv());
        fill(&ix).await;
        assert_eq!(ix.remove_many("i", &["d1"]).await.unwrap(), 1);
        assert!(
            holds(&plain_dir),
            "{name}: the control found no canary term after a plain removal"
        );
        drop(h);

        let h = registry::open_named(name, &Endpoint::path(erased_dir.join("k.db")), &needs())
            .await
            .unwrap();
        let ix = TextIndex::new(h.kv());
        fill(&ix).await;
        assert!(
            holds(&erased_dir),
            "{name}: the canary term was never written"
        );
        let before = ix.stats("i").await.unwrap();
        assert_eq!(
            ix.remove_many_erased("i", &["d1", "nope"]).await.unwrap(),
            1
        );
        assert!(
            !holds(&erased_dir),
            "{name}: the canary term survived remove_many_erased"
        );
        // Gone from the index, counted out, the rest untouched.
        assert!(
            ix.postings("i", CANARY, None, 10)
                .await
                .unwrap()
                .entries
                .is_empty()
        );
        assert_eq!(ix.doc_len("i", "d1").await.unwrap(), None);
        let after = ix.stats("i").await.unwrap();
        assert_eq!(
            (after.docs, after.total_len),
            (before.docs - 1, before.total_len - 4)
        );
        assert_eq!(ix.doc_len("i", "d2").await.unwrap(), Some(3));
        let hits = ix
            .search("i", &["shared".to_string()], Match::Any, 10)
            .await
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].doc_id, "d2");
        // Removing it again, or something that was never there, is Ok(0).
        assert_eq!(
            ix.remove_many_erased("i", &["d1", "nope"]).await.unwrap(),
            0
        );
        drop(h);
        let _ = std::fs::remove_dir_all(&plain_dir);
        let _ = std::fs::remove_dir_all(&erased_dir);
    }
}

#[tokio::test]
async fn memory_refuses_and_changes_nothing() {
    let kv = MemoryKv::new();
    let ix = TextIndex::new(&kv);
    ix.put("i", "d1", &terms(&[("a", 1)])).await.unwrap();
    let r = ix.remove_many_erased("i", &["d1"]).await;
    assert!(matches!(r, Err(KvError::Unsupported(_))), "{r:?}");
    assert_eq!(ix.doc_len("i", "d1").await.unwrap(), Some(1));
    assert_eq!(ix.stats("i").await.unwrap().docs, 1);
}
