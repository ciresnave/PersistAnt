// SPDX-License-Identifier: MIT OR Apache-2.0
//! A reader that holds a snapshot keeps a WAL checkpoint from truncating the log. An erased batch has
//! committed by then, so its erasure is incomplete and the call says so instead of returning `Ok`.
#![cfg(feature = "sqlite")]

use persistant_kv::{BatchOp, KvBackend, KvError, KvOnRelational};
use persistant_sqlx::SqliteBackend;

const CANARY: &[u8] = b"CANARY-4e92b7c0-not-a-secret";

#[tokio::test]
async fn a_held_reader_makes_the_erased_batch_report_the_untruncated_log() {
    let d = std::env::temp_dir().join(format!("persistant-batchreader-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    let path = d.join("k.db");
    let kv = KvOnRelational::open(SqliteBackend::open_durable(&path).await.unwrap())
        .await
        .unwrap();
    for i in 0..20 {
        kv.put(&format!("b/{i}"), vec![b'.'; 100], None)
            .await
            .unwrap();
    }
    kv.put("a", CANARY.to_vec(), None).await.unwrap();

    // A second pool of our own opens a read transaction and keeps it open.
    let reader = SqliteBackend::open_durable(&path).await.unwrap();
    let mut conn = reader.pool().acquire().await.unwrap();
    sqlx::query("BEGIN").execute(&mut *conn).await.unwrap();
    sqlx::query("SELECT count(*) FROM persistant_kv")
        .fetch_one(&mut *conn)
        .await
        .unwrap();

    let held = kv.batch(&[BatchOp::delete_erased("a")]).await;
    // Control: with the reader gone the same batch completes.
    sqlx::query("ROLLBACK").execute(&mut *conn).await.unwrap();
    drop(conn);
    kv.put("a", CANARY.to_vec(), None).await.unwrap();
    let free = kv.batch(&[BatchOp::delete_erased("a")]).await;

    match held {
        Err(KvError::Backend(m)) => assert!(m.contains("write-ahead log"), "{m}"),
        other => panic!("a held reader did not make the erased batch report: {other:?}"),
    }
    assert_eq!(
        free,
        Ok(true),
        "control: no reader, the erased batch completes"
    );
    assert_eq!(kv.get("a").await.unwrap(), None);
    drop(kv);
    let _ = std::fs::remove_dir_all(&d);
}
