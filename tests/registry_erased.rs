// SPDX-License-Identifier: MIT OR Apache-2.0
//! `delete_erased` and `execute_erased` through the registry's handles, async and blocking, bounded
//! and not: after an erased delete the canary is in none of the files, read back as raw bytes; the
//! same delete done plainly is the control where the control can fail (a WAL database keeps the
//! canary in its `-wal`; a file's bytes stay readable through a second hard link).
//!
//! The wrappers the registry builds (`Bounded`, the `Any*` enums) once dropped these calls to the
//! default `Unsupported`; the bounded cases here fail if any forward is removed.
#![cfg(feature = "registry-sqlite")]

use persistant::capability::CapabilityId;
use persistant::capability::choose::Durability;
use persistant::capability::provider::ProviderNeeds;
use persistant::kv::{KvBackend, KvError};
use persistant::registry::{self, Endpoint, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::Duration;

const CANARY: &[u8] = b"CANARY-61d2f0aa-not-a-secret";

fn id(s: &str) -> CapabilityId {
    CapabilityId::new(s).unwrap()
}

fn needs(extra: &[&str]) -> ProviderNeeds {
    let mut v = vec![id("kv.get"), id("kv.put"), id("kv.delete")];
    v.extend(extra.iter().map(|s| id(s)));
    ProviderNeeds::new(v, Durability::Volatile)
}

fn dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("persistant-regerase-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Whether any file under `d` (recursively) holds the canary.
fn holds(d: &Path) -> bool {
    std::fs::read_dir(d).unwrap().any(|e| {
        let p = e.unwrap().path();
        if p.is_dir() {
            holds(&p)
        } else {
            std::fs::read(&p)
                .unwrap()
                .windows(CANARY.len())
                .any(|w| w == CANARY)
        }
    })
}

async fn ballast<K: KvBackend>(kv: &K) {
    for i in 0..20 {
        kv.put(&format!("b/{i}"), vec![b'.'; 100], None)
            .await
            .unwrap();
    }
}

/// An SQLite store written plainly leaves the canary (in the `-wal` for `sqlite-full`, in the file
/// for `sqlite`): the control. An erased delete does not, through every handle shape.
#[tokio::test]
async fn async_handles_erase_on_sqlite_and_sqlite_full_bounded_or_not() {
    for name in ["sqlite", "sqlite-full"] {
        for bounded in [false, true] {
            let (plain_dir, erased_dir) = (
                dir(&format!("p-{name}-{bounded}")),
                dir(&format!("e-{name}-{bounded}")),
            );
            let extra: &[&str] = if bounded { &["kv.bounded_call"] } else { &[] };
            let opts = if bounded {
                OpenOptions::new().call_bound(Duration::from_secs(5))
            } else {
                OpenOptions::new()
            };
            let open = |d: &Path| {
                let n = needs(extra);
                let (d, o) = (d.join("k.db"), opts.clone());
                async move {
                    registry::open_named_with(name, &Endpoint::path(d), &n, &o)
                        .await
                        .unwrap()
                }
            };
            let h = open(&plain_dir).await;
            ballast(h.kv()).await;
            h.kv().put("u/1", CANARY.to_vec(), None).await.unwrap();
            h.kv().delete("u/1").await.unwrap();
            assert!(
                holds(&plain_dir),
                "{name} bounded={bounded}: the control found no canary after a plain delete"
            );
            drop(h);

            let h = open(&erased_dir).await;
            ballast(h.kv()).await;
            h.kv().put("u/1", CANARY.to_vec(), None).await.unwrap();
            h.kv().delete_erased("u/1").await.unwrap();
            assert_eq!(h.kv().get("u/1").await.unwrap(), None);
            assert!(
                !holds(&erased_dir),
                "{name} bounded={bounded}: the canary survived delete_erased"
            );
            drop(h);
            let _ = std::fs::remove_dir_all(&plain_dir);
            let _ = std::fs::remove_dir_all(&erased_dir);
        }
    }
}

#[tokio::test]
async fn async_handle_erases_on_fs_and_memory_says_unsupported() {
    let d = dir("fs");
    let h = registry::open_named("fs", &Endpoint::path(d.join("root")), &needs(&[]))
        .await
        .unwrap();
    h.kv().put("u/1", CANARY.to_vec(), None).await.unwrap();
    h.kv().delete_erased("u/1").await.unwrap();
    assert_eq!(h.kv().get("u/1").await.unwrap(), None);
    assert!(!holds(&d), "the canary survived delete_erased on fs");
    drop(h);
    let _ = std::fs::remove_dir_all(&d);

    let h = registry::open_named("memory", &Endpoint::None, &needs(&[]))
        .await
        .unwrap();
    h.kv().put("u/1", CANARY.to_vec(), None).await.unwrap();
    assert!(matches!(
        h.kv().delete_erased("u/1").await,
        Err(KvError::Unsupported(_))
    ));
    assert!(
        h.kv().get("u/1").await.unwrap().is_some(),
        "an unsupported erase removed nothing"
    );
}

#[test]
fn blocking_handles_erase_kv_blob_and_relational() {
    use persistant::blocking::registry as blocking;
    // kv on sqlite-full (WAL), plain delete is the control.
    for erased in [false, true] {
        let d = dir(&format!("bk-{erased}"));
        let h = blocking::open_named("sqlite-full", &Endpoint::path(d.join("k.db")), &needs(&[]))
            .unwrap();
        for i in 0..20 {
            h.put(&format!("b/{i}"), vec![b'.'; 100], None).unwrap();
        }
        h.put("u/1", CANARY.to_vec(), None).unwrap();
        if erased {
            h.delete_erased("u/1").unwrap();
        } else {
            h.delete("u/1").unwrap();
        }
        assert_eq!(h.get("u/1").unwrap(), None);
        assert_eq!(holds(&d), !erased, "kv erased={erased}");
        drop(h);
        let _ = std::fs::remove_dir_all(&d);
    }
    // blob on fs: the file bytes, read through a second link.
    let d = dir("bk-blob");
    let h = blocking::open_named(
        "fs",
        &Endpoint::path(d.join("root")),
        &needs(&["blob.delete"]),
    )
    .unwrap();
    let blob = h.blob().expect("the needs name a blob capability");
    blob.write("k/1", CANARY.to_vec()).unwrap();
    let alias = d.join("alias");
    let file = walk_one(&d.join("root"));
    std::fs::hard_link(&file, &alias).unwrap();
    assert!(
        std::fs::read(&alias)
            .unwrap()
            .windows(CANARY.len())
            .any(|w| w == CANARY)
    );
    blob.delete_erased("k/1").unwrap();
    assert!(blob.read("k/1").unwrap().is_none());
    assert!(
        !std::fs::read(&alias)
            .unwrap()
            .windows(CANARY.len())
            .any(|w| w == CANARY),
        "the blob's bytes survived delete_erased"
    );
    drop(h);
    let _ = std::fs::remove_dir_all(&d);
    // relational on sqlite (rollback journal; `sqlite-full` with a `rel.*` need is Unmet `NoRow` in the registry today).
    for erased in [false, true] {
        let d = dir(&format!("bk-rel-{erased}"));
        let h = blocking::open_named(
            "sqlite",
            &Endpoint::path(d.join("k.db")),
            &ProviderNeeds::new(vec![id("rel.values")], Durability::Volatile),
        )
        .unwrap();
        let rel = h.rel().expect("the needs name a rel capability");
        rel.execute("CREATE TABLE t (k INTEGER PRIMARY KEY, v BLOB)", &[])
            .unwrap();
        for i in 0..20 {
            rel.execute(
                "INSERT INTO t VALUES ($1, $2)",
                &[
                    persistant::relational::Value::Int(i),
                    persistant::relational::Value::Bytes(vec![b'.'; 100]),
                ],
            )
            .unwrap();
        }
        rel.execute(
            "INSERT INTO t VALUES (100, $1)",
            &[persistant::relational::Value::Bytes(CANARY.to_vec())],
        )
        .unwrap();
        let n = if erased {
            rel.execute_erased("DELETE FROM t WHERE k = 100", &[])
                .unwrap()
        } else {
            rel.execute("DELETE FROM t WHERE k = 100", &[]).unwrap()
        };
        assert_eq!(n, 1);
        assert_eq!(holds(&d), !erased, "rel erased={erased}");
        drop(h);
        let _ = std::fs::remove_dir_all(&d);
    }
}

fn walk_one(d: &Path) -> PathBuf {
    let mut found = Vec::new();
    let mut stack = vec![d.to_path_buf()];
    while let Some(x) = stack.pop() {
        for e in std::fs::read_dir(x).unwrap() {
            let e = e.unwrap();
            if e.file_type().unwrap().is_dir() {
                stack.push(e.path());
            } else {
                found.push(e.path());
            }
        }
    }
    assert_eq!(found.len(), 1, "{found:?}");
    found.remove(0)
}
