// SPDX-License-Identifier: MIT OR Apache-2.0
//! `persistant::update_record`: the typed read-modify-write over compare-and-swap (row 41, step 1).
#![cfg(feature = "kv")]

use persistant::kv::{Defect, KvBackend, KvError, KvOnBlob, MemoryKv};
use persistant::{Record, RecordUpdateError, update_record};
use serde::{Deserialize, Serialize};

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct Counter {
    n: u32,
}

impl Record for Counter {
    const KIND: &'static str = "counter";
    const SCHEMA: u32 = 1;
}

#[derive(Debug, Serialize, Deserialize)]
struct Other {
    n: u32,
}

impl Record for Other {
    const KIND: &'static str = "other";
    const SCHEMA: u32 = 1;
}

fn bump(c: Option<Counter>) -> Option<Counter> {
    Some(Counter {
        n: c.map_or(0, |c| c.n) + 1,
    })
}

async fn total(kv: &MemoryKv, key: &str) -> u32 {
    let bytes = kv.get(key).await.unwrap().unwrap();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    v["data"]["n"].as_u64().unwrap() as u32
}

async fn race(kv: &MemoryKv, key: &str, writers: usize) {
    let tasks: Vec<_> = (0..writers)
        .map(|_| async {
            update_record::<_, Counter, _>(kv, key, None, bump)
                .await
                .unwrap()
        })
        .collect();
    for ok in futures_util::future::join_all(tasks).await {
        assert!(ok);
    }
}

#[tokio::test]
async fn sixteen_concurrent_writers_land_exactly_sixteen_updates() {
    let kv = MemoryKv::new();
    race(&kv, "c", 16).await;
    assert_eq!(total(&kv, "c").await, 16);
}

/// The known-bad control: a backend whose compare-and-swap does not compare loses updates, so the
/// total test above can fail (without this it would prove nothing).
#[tokio::test]
async fn a_cas_that_ignores_the_expected_value_loses_updates() {
    let kv = MemoryKv::with_defect(Defect::CasIgnoresExpected);
    race(&kv, "c", 16).await;
    assert!(total(&kv, "c").await < 16);
}

#[tokio::test]
async fn an_absent_key_is_none_and_a_declined_update_writes_nothing() {
    let kv = MemoryKv::new();
    let mut saw_none = false;
    let committed = update_record::<_, Counter, _>(&kv, "k", None, |c| {
        saw_none = c.is_none();
        None
    })
    .await
    .unwrap();
    assert!(!committed && saw_none);
    assert_eq!(kv.get("k").await.unwrap(), None);
}

#[tokio::test]
async fn a_record_of_another_kind_is_refused_and_left_alone() {
    let kv = MemoryKv::new();
    update_record::<_, Other, _>(&kv, "k", None, |_| Some(Other { n: 7 }))
        .await
        .unwrap();
    let before = kv.get("k").await.unwrap();
    let r = update_record::<_, Counter, _>(&kv, "k", None, bump).await;
    assert!(matches!(r, Err(RecordUpdateError::Record(_))), "{r:?}");
    assert_eq!(kv.get("k").await.unwrap(), before);
}

#[tokio::test]
async fn a_blob_store_is_refused_with_a_typed_error() {
    let kv = KvOnBlob::new(persistant::blob::MemoryBlob::new());
    let r = update_record::<_, Counter, _>(&kv, "k", None, bump).await;
    assert!(
        matches!(r, Err(RecordUpdateError::Kv(KvError::Unsupported(_)))),
        "{r:?}"
    );
}
