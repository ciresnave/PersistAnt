// SPDX-License-Identifier: MIT OR Apache-2.0
//! `blocking::registry::KvHandle::text()`: the text index for synchronous callers, on `sqlite-secure`.
//! Put, read and search round-trip; `remove_prefix_erased` removes every document under an id prefix
//! (more than one round of 100), and nothing outside it (`"a/"` is not `"ab/"`), with the removed
//! documents' terms in none of the database's files (a plain `remove_many` is the control: it leaves
//! the canary in them); the totals follow; a repeat is `Ok(0)`.
#![cfg(all(feature = "text-index", feature = "registry-sqlite"))]

use persistant::blocking::registry::{self, KvHandle};
use persistant::capability::CapabilityId;
use persistant::capability::choose::Durability;
use persistant::capability::provider::ProviderNeeds;
use persistant::kv::text::Match;
use persistant::registry::Endpoint;
use std::path::{Path, PathBuf};

const CANARY: &str = "zzcanaryterm5c2e10";

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
    let d = std::env::temp_dir().join(format!("persistant-blktext-{}-{tag}", std::process::id()));
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

fn open(d: &Path, name: &str) -> KvHandle {
    registry::open_named(name, &Endpoint::path(d.join("k.db")), &needs()).unwrap()
}

/// 250 versions under `a/` (the first carries the canary), one under `ab/` and one under `b/`.
fn fill(h: &KvHandle) {
    let t = h.text();
    for i in 0..250 {
        let words = if i == 0 {
            terms(&[(CANARY, 3), ("common", 1)])
        } else {
            terms(&[("common", 1)])
        };
        t.put("i", &format!("a/{i:03}"), &words).unwrap();
    }
    t.put("i", "ab/1", &terms(&[("common", 2), ("keepab", 1)]))
        .unwrap();
    t.put("i", "b/1", &terms(&[("keepb", 5)])).unwrap();
}

#[test]
fn the_blocking_text_index_reads_and_searches() {
    let d = dir("rw");
    let h = open(&d, "sqlite-secure");
    fill(&h);
    let t = h.text();
    let s = t.stats("i").unwrap();
    assert_eq!((s.docs, s.total_len), (252, 4 + 249 + 3 + 5));
    assert_eq!(t.doc_len("i", "b/1").unwrap(), Some(5));
    assert_eq!(t.postings("i", "keepb", None, 10).unwrap().entries.len(), 1);
    let hits = t
        .search("i", &["keepab".to_string()], Match::Any, 10)
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].doc_id, "ab/1");
    let page = t.docs("i", None, 3).unwrap();
    assert_eq!(page.ids, ["a/000", "a/001", "a/002"]);
    let under = t.docs_with_prefix("i", "a/", Some("a/248"), 10).unwrap();
    assert_eq!(under.ids, ["a/249"], "ab/1 is not under a/");
    assert!(
        t.docs_with_prefix("i", "zz/", None, 10)
            .unwrap()
            .ids
            .is_empty()
    );
    drop(h);
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn remove_prefix_erased_removes_the_prefix_and_its_terms_and_nothing_else() {
    // The control: on plain `sqlite` (no secure_delete), a plain removal of the canary's document
    // leaves the canary in the files.
    let cd = dir("ctl");
    let h = open(&cd, "sqlite");
    fill(&h);
    assert!(holds(&cd), "the canary was never written");
    assert_eq!(h.text().remove_many("i", &["a/000"]).unwrap(), 1);
    drop(h);
    let plain_kept = holds(&cd);
    let _ = std::fs::remove_dir_all(&cd);

    let d = dir("pfx");
    let h = open(&d, "sqlite-secure");
    fill(&h);
    let t = h.text();
    assert_eq!(t.remove_prefix_erased("i", "a/").unwrap(), 250);
    assert!(!holds(&d), "the canary term survived remove_prefix_erased");
    assert!(
        plain_kept,
        "the control: a plain remove_many on sqlite left no canary, so the check above proves nothing"
    );
    let s = t.stats("i").unwrap();
    assert_eq!((s.docs, s.total_len), (2, 3 + 5));
    assert_eq!(t.doc_len("i", "ab/1").unwrap(), Some(3));
    assert_eq!(t.doc_len("i", "a/100").unwrap(), None);
    assert_eq!(
        t.postings("i", "common", None, 10).unwrap().entries.len(),
        1
    );
    // A repeat, and a prefix nobody has, remove nothing.
    assert_eq!(t.remove_prefix_erased("i", "a/").unwrap(), 0);
    assert_eq!(t.remove_prefix_erased("i", "nobody/").unwrap(), 0);
    drop(h);
    let _ = std::fs::remove_dir_all(&d);
}
