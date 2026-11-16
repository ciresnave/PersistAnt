// SPDX-License-Identifier: MIT OR Apache-2.0
//! `text.index` certification (`persistant_kv::text_conformance`): on for a correct backend with
//! both controls held, off as wrong for a backend whose batch ignores its `Require` or is not
//! applied as a unit, off as missing for a blob store.
#![cfg(feature = "text-index")]

use persistant::capability::{Config, Reason};
use persistant::kv::text_conformance::{Failure, certify, run};
use persistant::kv::{Defect, KvOnBlob, MemoryKv};

#[tokio::test]
async fn certify_is_on_for_a_correct_backend_with_both_controls() {
    let kv = MemoryKv::new();
    let row = certify(&kv, "memory", "0", &Config::new(), &Config::new()).await;
    assert!(row.enabled, "{row:?}");
    assert_eq!(row.subject.as_str(), "text.index");
    assert_eq!(row.metadata.get("controls"), None);
}

#[tokio::test]
async fn each_known_bad_backend_fails_the_checks() {
    for d in [Defect::BatchIgnoresRequire, Defect::BatchNotAtomic] {
        let r = run(&MemoryKv::with_defect(d), "p/").await;
        assert!(matches!(r, Err(Failure::Wrong(_))), "{d:?}: {r:?}");
    }
    // And through certify: off, wrong, with the reading in the note.
    let kv = MemoryKv::with_defect(Defect::BatchIgnoresRequire);
    let row = certify(&kv, "memory", "0", &Config::new(), &Config::new()).await;
    assert!(!row.enabled);
    assert_eq!(row.reason, Some(Reason::Wrong));
    assert!(!row.note.is_empty());
}

#[tokio::test]
async fn a_blob_store_is_off_as_missing() {
    let kv = KvOnBlob::new(persistant::blob::MemoryBlob::new());
    let row = certify(
        &kv,
        "kv-on-blob:memory",
        "0",
        &Config::new(),
        &Config::new(),
    )
    .await;
    assert!(!row.enabled);
    assert_eq!(row.reason, Some(Reason::Missing), "{row:?}");
}

/// `erased_unsupported` (the matrix generators' probe): a kv that cannot erase inside a batch is
/// reported with its words and nothing is left behind; a blob store, which cannot even `put`, too.
#[tokio::test]
async fn the_erased_probe_reports_unsupported_and_leaves_nothing() {
    use persistant::kv::text::TextIndex;
    use persistant::kv::text_conformance::erased_unsupported;
    let kv = MemoryKv::new();
    let why = erased_unsupported(&kv, "p/").await.unwrap();
    assert!(
        why.as_deref()
            .is_some_and(|m| m.contains("does not support")),
        "{why:?}"
    );
    let ix = TextIndex::new(&kv);
    assert_eq!(ix.stats("p/erased-probe").await.unwrap().docs, 0);
    assert!(
        ix.docs("p/erased-probe", None, 10)
            .await
            .unwrap()
            .ids
            .is_empty()
    );

    let blob = KvOnBlob::new(persistant::blob::MemoryBlob::new());
    let why = erased_unsupported(&blob, "p/").await.unwrap();
    assert!(why.is_some(), "{why:?}");
}
