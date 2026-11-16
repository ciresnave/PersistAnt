// SPDX-License-Identifier: MIT OR Apache-2.0
//! `kv.delete_erased` and `TextIndex::remove_many_erased` with many records (N = 30), raw-file
//! read-back on each SQLite profile. The erased calls zero what they free, but stale in-page
//! fragments of earlier writes (left by page splits and rebalancing while `secure_delete` was off)
//! are not reached: `sqlite` and `sqlite-full` keep some, only `sqlite-secure` (`secure_delete` on
//! for every connection) keeps none. The plain removal is the control on every profile that is not
//! secure.
#![cfg(all(feature = "text-index", feature = "registry-sqlite"))]

use persistant::capability::CapabilityId;
use persistant::capability::choose::Durability;
use persistant::capability::provider::ProviderNeeds;
use persistant::kv::text::TextIndex;
use persistant::registry::{self, Endpoint};
use std::path::{Path, PathBuf};

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
    let d = std::env::temp_dir().join(format!(
        "persistant-texterasen-{}-{tag}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn copies(d: &Path, needle: &str) -> usize {
    let mut n = 0;
    for e in std::fs::read_dir(d).unwrap() {
        let b = std::fs::read(e.unwrap().path()).unwrap();
        n += b
            .windows(needle.len())
            .filter(|w| *w == needle.as_bytes())
            .count();
    }
    n
}

fn term(i: usize) -> String {
    format!("zqxmarker{i:03}")
}

async fn run(name: &str, erased: bool, n: usize) -> usize {
    let d = dir(&format!("{name}-{erased}-{n}"));
    let h = registry::open_named(name, &Endpoint::path(d.join("k.db")), &needs())
        .await
        .unwrap();
    let ix = TextIndex::new(h.kv());
    let mut ids = Vec::new();
    for i in 0..n {
        let doc = format!("d{i:03}");
        ix.put("i", &doc, &[(term(i), 1), (format!("pad{i}"), 1)])
            .await
            .unwrap();
        ids.push(doc);
    }
    let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
    if erased {
        ix.remove_many_erased("i", &refs).await.unwrap();
    } else {
        ix.remove_many("i", &refs).await.unwrap();
    }
    drop(h);
    (0..n).map(|i| copies(&d, &term(i))).sum()
}

async fn run_kv(name: &str, n: usize) -> usize {
    use persistant::kv::KvBackend;
    let d = dir(&format!("kv-{name}-{n}"));
    let h = registry::open_named(name, &Endpoint::path(d.join("k.db")), &needs())
        .await
        .unwrap();
    let kv = h.kv();
    for i in 0..n {
        let mut v = format!("text {} ", term(i)).into_bytes();
        v.resize(700, b'.');
        kv.put(&format!("c/{i:010}/0000000001"), v, None)
            .await
            .unwrap();
    }
    for i in 0..n {
        kv.delete_erased(&format!("c/{i:010}/0000000001"))
            .await
            .unwrap();
    }
    drop(h);
    (0..n).map(|i| copies(&d, &term(i))).sum()
}

async fn run_heavy(name: &str, n: usize) -> usize {
    let d = dir(&format!("heavy-{name}-{n}"));
    let h = registry::open_named(name, &Endpoint::path(d.join("k.db")), &needs())
        .await
        .unwrap();
    let ix = TextIndex::new(h.kv());
    let mut ids = Vec::new();
    for i in 0..n {
        let doc = format!("d{i:03}");
        let mut t = vec![(term(i), 1u32)];
        for j in 0..40 {
            t.push((format!("common{j}"), 1));
        }
        ix.put("i", &doc, &t).await.unwrap();
        ids.push(doc);
    }
    let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
    ix.remove_many_erased("i", &refs).await.unwrap();
    drop(h);
    (0..n).map(|i| copies(&d, &term(i))).sum()
}

/// The known-bad control: a plain removal leaves the terms in the files (every profile that is not
/// secure), so a count of zero below means something.
#[tokio::test]
async fn a_plain_removal_leaves_the_terms() {
    for name in ["sqlite", "sqlite-full"] {
        assert!(run(name, false, 30).await > 0, "{name}: no control hits");
    }
}

#[tokio::test]
async fn the_secure_profile_leaves_no_term_after_an_erased_removal() {
    assert_eq!(run("sqlite-secure", true, 30).await, 0);
    assert_eq!(run_kv("sqlite-secure", 30).await, 0);
    assert_eq!(run_heavy("sqlite-secure", 30).await, 0);
}

/// The limit, stated as a measurement: with many records the plain and the durable profile keep
/// stale in-page fragments after an erased call (the matrix records `kv.delete_erased` and
/// `text.index.erased` off, wrong, there). If this starts to fail the fragments are gone: update the
/// rows and the docs.
#[tokio::test]
async fn the_plain_and_durable_profiles_keep_stale_fragments() {
    for name in ["sqlite", "sqlite-full"] {
        let left = run_kv(name, 30).await + run_heavy(name, 30).await;
        assert!(left > 0, "{name}: no stale fragments found at N=30");
    }
}
