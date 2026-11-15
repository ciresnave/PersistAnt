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
