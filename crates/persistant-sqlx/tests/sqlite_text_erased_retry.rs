// SPDX-License-Identifier: MIT OR Apache-2.0
//! `TextIndex::remove_many_erased` when a reader keeps the write-ahead log from being truncated: the
//! call returns an error after its batch has committed (the documents are gone), and calling it again
//! with the same ids finishes the job: the term is in none of the files afterwards. Without the repeat
//! step the second call would return `Ok(0)` and leave the term in the log.
#![cfg(feature = "sqlite")]

use persistant_kv::text::TextIndex;
use persistant_kv::{KvError, KvOnRelational};
use persistant_sqlx::SqliteBackend;

const CANARY: &str = "zzcanaryterm4c19e0";

fn holds(d: &std::path::Path) -> bool {
    std::fs::read_dir(d).unwrap().any(|e| {
        std::fs::read(e.unwrap().path())
            .unwrap()
            .windows(CANARY.len())
            .any(|w| w == CANARY.as_bytes())
    })
}

#[tokio::test]
async fn a_repeat_after_a_held_reader_finishes_the_erasure() {
    let d = std::env::temp_dir().join(format!("persistant-texteraseretry-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    let path = d.join("k.db");
    let kv = KvOnRelational::open(SqliteBackend::open_durable(&path).await.unwrap())
        .await
        .unwrap();
    let ix = TextIndex::new(&kv);
    for i in 0..20 {
        ix.put("i", &format!("f{i:02}"), &[("filler".to_string(), 1)])
            .await
            .unwrap();
    }
    ix.put("i", "d1", &[(CANARY.to_string(), 2)]).await.unwrap();

    let reader = SqliteBackend::open_durable(&path).await.unwrap();
    let mut conn = reader.pool().acquire().await.unwrap();
    sqlx::query("BEGIN").execute(&mut *conn).await.unwrap();
    sqlx::query("SELECT count(*) FROM persistant_kv")
        .fetch_one(&mut *conn)
        .await
        .unwrap();

    let held = ix.remove_many_erased("i", &["d1"]).await;
    match held {
        Err(KvError::Backend(m)) => assert!(m.contains("write-ahead log"), "{m}"),
        other => panic!("a held reader did not make the erased removal report: {other:?}"),
    }
    // The batch committed: the document is gone from the index, its term still in the log.
    assert_eq!(ix.doc_len("i", "d1").await.unwrap(), None);
    assert!(
        holds(&d),
        "control: the term should still be in the log while the reader holds it"
    );

    sqlx::query("ROLLBACK").execute(&mut *conn).await.unwrap();
    drop(conn);
    // The repeat: nothing left to remove, and the erased step truncates the log.
    assert_eq!(ix.remove_many_erased("i", &["d1"]).await.unwrap(), 0);
    assert!(!holds(&d), "the term survived the repeat call");
    drop(kv);
    let _ = std::fs::remove_dir_all(&d);
}
