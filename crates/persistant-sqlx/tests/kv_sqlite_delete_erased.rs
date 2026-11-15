// SPDX-License-Identifier: MIT OR Apache-2.0
//! `kv.delete_erased` over `KvOnRelational` on SQLite: after it, a canary value is in none of the
//! database file, `-wal` or `-journal`, read as raw bytes. A plain `kv.delete` is the control.
//!
//! **Not covered:** a value overwritten earlier by a plain `put` (its old bytes may sit in free
//! space inside a page), and anything a file system or backup keeps.
#![cfg(feature = "sqlite")]

use persistant_kv::{KvBackend, KvOnRelational};
use persistant_sqlx::SqliteBackend;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);
const CANARY: &[u8] = b"CANARY-0c8e5d11-not-a-secret";

async fn trial(durable: bool, erased: bool) -> Vec<String> {
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("persistant-kverase-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("kv.sqlite");
    let db = if durable {
        SqliteBackend::open_durable(&path).await.unwrap()
    } else {
        SqliteBackend::open(&path).await.unwrap()
    };
    let kv = KvOnRelational::open(db).await.unwrap();
    for i in 0..20 {
        kv.put(&format!("ballast/{i}"), vec![b'.'; 100], None)
            .await
            .unwrap();
    }
    kv.put("user/1", CANARY.to_vec(), None).await.unwrap();
    if erased {
        kv.delete_erased("user/1").await.unwrap();
        // An absent key is Ok.
        kv.delete_erased("user/absent").await.unwrap();
    } else {
        kv.delete("user/1").await.unwrap();
    }
    assert_eq!(kv.get("user/1").await.unwrap(), None);
    // The other keys are still there.
    assert!(kv.get("ballast/3").await.unwrap().is_some());
    let mut hits = Vec::new();
    for e in std::fs::read_dir(&dir).unwrap() {
        let p = e.unwrap().path();
        if std::fs::read(&p)
            .unwrap()
            .windows(CANARY.len())
            .any(|w| w == CANARY)
        {
            hits.push(p.file_name().unwrap().to_string_lossy().into_owned());
        }
    }
    drop(kv);
    let _ = std::fs::remove_dir_all(&dir);
    hits
}

#[tokio::test]
async fn plain_kv_delete_leaves_the_canary() {
    for durable in [false, true] {
        assert!(
            !trial(durable, false).await.is_empty(),
            "durable={durable}: the control found nothing"
        );
    }
}

#[tokio::test]
async fn kv_delete_erased_leaves_no_canary() {
    for durable in [false, true] {
        let hits = trial(durable, true).await;
        assert!(hits.is_empty(), "durable={durable}: canary in {hits:?}");
    }
}
