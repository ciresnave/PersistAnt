// SPDX-License-Identifier: MIT OR Apache-2.0
//! The files `SqliteBackend::open` and `open_durable` create are owner-only on Unix, whatever the
//! process umask, and a file that was already there keeps the mode its owner gave it. (A per-tenant
//! database holds secrets; SQLite's own default is 0644 less the umask, which is world-readable under
//! the usual 022.)
#![cfg(unix)]

use persistant_relational::{RelationalBackend, Value};
use persistant_sqlx::SqliteBackend;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

fn mode(path: &Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("persistant-mode-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

async fn write_one(db: &SqliteBackend) {
    db.execute("CREATE TABLE t (n INTEGER)", &[]).await.unwrap();
    db.execute("INSERT INTO t VALUES ($1)", &[Value::Int(1)])
        .await
        .unwrap();
}

/// The database file, and the `-wal` and `-shm` files beside it: those must exist when `wal` (WAL mode
/// keeps them while the database is open); any that exist are owner-only either way.
fn owner_only(file: &Path, what: &str, wal: bool) {
    for suffix in ["", "-wal", "-shm"] {
        let mut name = file.as_os_str().to_owned();
        name.push(suffix);
        let p = std::path::PathBuf::from(name);
        if suffix.is_empty() || wal {
            assert!(
                p.exists(),
                "{what}: {} should exist while open",
                p.display()
            );
        } else if !p.exists() {
            continue;
        }
        assert_eq!(
            mode(&p) & 0o077,
            0,
            "{what}: {} is {:o}, group or other can read it",
            p.display(),
            mode(&p)
        );
    }
}

#[tokio::test]
async fn a_new_database_and_its_wal_files_are_owner_only() {
    let dir = scratch("open");
    let file = dir.join("tenant.sqlite");
    let db = SqliteBackend::open(&file).await.unwrap();
    write_one(&db).await;
    owner_only(&file, "open", false);
    drop(db);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn a_new_durable_database_and_its_wal_files_are_owner_only() {
    let dir = scratch("durable");
    let file = dir.join("tenant.sqlite");
    let db = SqliteBackend::open_durable(&file).await.unwrap();
    write_one(&db).await;
    owner_only(&file, "open_durable", true);
    drop(db);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn an_existing_file_keeps_its_mode() {
    let dir = scratch("existing");
    let file = dir.join("shared.sqlite");
    std::fs::write(&file, b"").unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o640)).unwrap();
    let db = SqliteBackend::open(&file).await.unwrap();
    write_one(&db).await;
    assert_eq!(
        mode(&file),
        0o640,
        "an operator's chosen mode is not changed"
    );
    drop(db);
    let _ = std::fs::remove_dir_all(&dir);
}
