// SPDX-License-Identifier: MIT OR Apache-2.0
//! What `SqliteBackend::open` does with a path that is not there yet (a per-tenant database opened on
//! first use): it creates the file, it does not create the directory, and a second open finds the first
//! one's data. The file's mode is SQLite's own and follows the umask; see the docs of `open`.

use persistant_relational::{RelationalBackend, Value};
use persistant_sqlx::SqliteBackend;

#[tokio::test]
async fn a_missing_file_is_created_and_a_missing_directory_is_not() {
    let dir = std::env::temp_dir().join(format!("persistant-open-path-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("tenant-a.sqlite");
    assert!(!file.exists());

    let db = SqliteBackend::open(&file).await.unwrap();
    assert!(file.exists(), "open created the file");
    db.execute("CREATE TABLE t (n INTEGER)", &[]).await.unwrap();
    db.execute("INSERT INTO t VALUES ($1)", &[Value::Int(7)])
        .await
        .unwrap();
    // `open` leaves SQLite's journal mode alone (a rollback journal); `open_durable` asks for WAL.
    let mode = db.query("PRAGMA journal_mode", &[]).await.unwrap();
    assert_eq!(mode[0].text(0), Some("delete"));
    drop(db);

    let again = SqliteBackend::open(&file).await.unwrap();
    let rows = again.query("SELECT n FROM t", &[]).await.unwrap();
    assert_eq!(rows.len(), 1, "a second open sees the first one's data");
    drop(again);

    let nested = dir.join("no-such-dir").join("tenant-b.sqlite");
    assert!(
        SqliteBackend::open(&nested).await.is_err(),
        "a missing parent directory is an error"
    );
    assert!(!nested.parent().unwrap().exists(), "and is not created");

    let _ = std::fs::remove_dir_all(&dir);
}
