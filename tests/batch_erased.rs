// SPDX-License-Identifier: MIT OR Apache-2.0
//! `BatchOp::DeleteErased`: an erased delete inside an atomic batch. On SQLite the batch is one
//! transaction with `secure_delete` on and the write-ahead log is truncated after the commit, so
//! after it the canary is in none of the files (read back as raw bytes) and the rest of the batch
//! took effect with it; a refused `Require` changes nothing; a backend that cannot (memory) refuses
//! the whole batch before anything changes; a reader that keeps the log from being truncated makes
//! the batch report it rather than claim erasure.
#![cfg(feature = "registry-sqlite")]

use persistant::capability::CapabilityId;
use persistant::capability::choose::Durability;
use persistant::capability::provider::ProviderNeeds;
use persistant::kv::{BatchOp, KvBackend, KvError};
use persistant::registry::{self, Endpoint, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::Duration;

const CANARY: &[u8] = b"CANARY-a07c55e1-not-a-secret";

fn needs(extra: &[&str]) -> ProviderNeeds {
    let id = |s: &str| CapabilityId::new(s).unwrap();
    let mut v = vec![
        id("kv.get"),
        id("kv.put"),
        id("kv.delete"),
        id("kv.batch_atomic.rollback"),
    ];
    v.extend(extra.iter().map(|s| id(s)));
    ProviderNeeds::new(v, Durability::Volatile)
}

fn dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "persistant-batcherase-{}-{tag}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn holds(d: &Path) -> bool {
    std::fs::read_dir(d).unwrap().any(|e| {
        std::fs::read(e.unwrap().path())
            .unwrap()
            .windows(CANARY.len())
            .any(|w| w == CANARY)
    })
}

async fn seed<K: KvBackend>(kv: &K) {
    for i in 0..20 {
        kv.put(&format!("b/{i}"), vec![b'.'; 100], None)
            .await
            .unwrap();
    }
    kv.put("a", CANARY.to_vec(), None).await.unwrap();
    kv.put("d", b"plain".to_vec(), None).await.unwrap();
}

#[tokio::test]
async fn an_erased_batch_erases_and_applies_the_rest_atomically() {
    for name in ["sqlite", "sqlite-full"] {
        for bounded in [false, true] {
            let tag = format!("{name}-{bounded}");
            let (plain_dir, erased_dir) = (dir(&format!("p-{tag}")), dir(&format!("e-{tag}")));
            let extra: &[&str] = if bounded { &["kv.bounded_call"] } else { &[] };
            let opts = if bounded {
                OpenOptions::new().call_bound(Duration::from_secs(5))
            } else {
                OpenOptions::new()
            };
            let open = |d: &Path| {
                let (n, d, o) = (needs(extra), d.join("k.db"), opts.clone());
                async move {
                    registry::open_named_with(name, &Endpoint::path(d), &n, &o)
                        .await
                        .unwrap()
                }
            };
            // The control: the same batch with a plain delete leaves the canary in the files.
            let h = open(&plain_dir).await;
            seed(h.kv()).await;
            assert!(
                h.kv()
                    .batch(&[
                        BatchOp::require("a", Some(CANARY.to_vec())),
                        BatchOp::delete("a")
                    ])
                    .await
                    .unwrap()
            );
            assert!(
                holds(&plain_dir),
                "{tag}: the control found no canary after a plain batch delete"
            );
            drop(h);

            let h = open(&erased_dir).await;
            seed(h.kv()).await;
            // A refused precondition changes nothing, erased delete included.
            let refused = h
                .kv()
                .batch(&[
                    BatchOp::require("a", Some(b"not it".to_vec())),
                    BatchOp::delete_erased("a"),
                    BatchOp::put("c", b"x".to_vec()),
                ])
                .await
                .unwrap();
            assert!(!refused, "{tag}: a refused Require was applied");
            assert_eq!(h.kv().get("a").await.unwrap().as_deref(), Some(CANARY));
            assert_eq!(h.kv().get("c").await.unwrap(), None);
            // The real one. No put in it: a put into the same page would overwrite the freed bytes
            // and hide a missing `secure_delete`.
            let applied = h
                .kv()
                .batch(&[
                    BatchOp::require("a", Some(CANARY.to_vec())),
                    BatchOp::delete_erased("a"),
                    BatchOp::delete("d"),
                ])
                .await
                .unwrap();
            assert!(applied, "{tag}");
            assert_eq!(h.kv().get("a").await.unwrap(), None);
            assert_eq!(h.kv().get("d").await.unwrap(), None);
            assert!(
                !holds(&erased_dir),
                "{tag}: the canary survived an erased batch"
            );
            // An erased delete mixes with other operations in one batch.
            let mixed = h
                .kv()
                .batch(&[
                    BatchOp::delete_erased("b/0"),
                    BatchOp::put("c", b"x".to_vec()),
                    BatchOp::delete("b/1"),
                ])
                .await
                .unwrap();
            assert!(mixed, "{tag}");
            assert_eq!(h.kv().get("c").await.unwrap().as_deref(), Some(&b"x"[..]));
            assert_eq!(h.kv().get("b/0").await.unwrap(), None);
            assert_eq!(h.kv().get("b/1").await.unwrap(), None);
            drop(h);
            let _ = std::fs::remove_dir_all(&plain_dir);
            let _ = std::fs::remove_dir_all(&erased_dir);
        }
    }
}

#[tokio::test]
async fn memory_refuses_the_whole_batch_and_changes_nothing() {
    use persistant::kv::MemoryKv;
    let kv = MemoryKv::new();
    kv.put("a", b"v".to_vec(), None).await.unwrap();
    let r = kv
        .batch(&[
            BatchOp::put("c", b"x".to_vec()),
            BatchOp::delete_erased("a"),
        ])
        .await;
    assert!(matches!(r, Err(KvError::Unsupported(_))), "{r:?}");
    assert_eq!(kv.get("a").await.unwrap().as_deref(), Some(&b"v"[..]));
    assert_eq!(
        kv.get("c").await.unwrap(),
        None,
        "the put before it was applied"
    );
}

#[test]
fn the_blocking_handle_takes_an_erased_batch() {
    use persistant::blocking::registry as blocking;
    let d = dir("blocking");
    let h =
        blocking::open_named("sqlite-full", &Endpoint::path(d.join("k.db")), &needs(&[])).unwrap();
    for i in 0..20 {
        h.put(&format!("b/{i}"), vec![b'.'; 100], None).unwrap();
    }
    h.put("a", CANARY.to_vec(), None).unwrap();
    assert!(h.batch(&[BatchOp::delete_erased("a")]).unwrap());
    assert_eq!(h.get("a").unwrap(), None);
    assert!(!holds(&d));
    drop(h);
    let _ = std::fs::remove_dir_all(&d);
}
