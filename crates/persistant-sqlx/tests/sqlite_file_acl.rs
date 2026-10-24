// SPDX-License-Identifier: MIT OR Apache-2.0
//! On Windows the files `SqliteBackend::open` and `open_durable` create carry a protected ACL with
//! one entry, for the user the process runs as; a file that was already there keeps its ACL. (A new
//! file would otherwise inherit its directory's ACL, which usually lets other local users read it.)
//!
//! The ACL is read back with `icacls`, a different route from the one that sets it (the Win32
//! security API), and every check has a control: an ordinary file in the same directory is not
//! owner-only by the same reading.
#![cfg(windows)]

use persistant_relational::{RelationalBackend, Value};
use persistant_sqlx::SqliteBackend;
use std::path::Path;
use std::process::Command;

fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("persistant-acl-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn run(cmd: &str, args: &[&str]) -> String {
    let out = Command::new(cmd).args(args).output().unwrap();
    assert!(out.status.success(), "{cmd} {args:?}: {out:?}");
    String::from_utf8_lossy(&out.stdout).to_string()
}

/// `domain\user` of the account running the test, lower-cased.
fn me() -> String {
    run("whoami", &[]).trim().to_lowercase()
}

/// The ACL entries of `path` as `icacls` prints them: `principal:(rights)`, without the path.
fn entries(path: &Path) -> Vec<String> {
    let text = run("icacls", &[&path.to_string_lossy()]);
    let mut found = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with("Successfully processed") {
            continue;
        }
        // The first line starts with the path; later lines are indented entries.
        let entry = if i == 0 {
            line.strip_prefix(&*path.to_string_lossy())
                .unwrap_or(line)
                .trim()
        } else {
            line
        };
        if entry.contains(":(") {
            found.push(entry.to_lowercase());
        }
    }
    found
}

fn only_me(path: &Path, what: &str) {
    let e = entries(path);
    assert_eq!(e.len(), 1, "{what}: {} has {e:?}", path.display());
    assert!(
        e[0].starts_with(&format!("{}:", me())) && e[0].contains("(f)") && !e[0].contains("(i)"),
        "{what}: {} is {e:?}, want full control for {} and nothing inherited",
        path.display(),
        me()
    );
}

async fn write_one(db: &SqliteBackend) {
    db.execute("CREATE TABLE t (n INTEGER)", &[]).await.unwrap();
    db.execute("INSERT INTO t VALUES ($1)", &[Value::Int(1)])
        .await
        .unwrap();
}

#[tokio::test]
async fn the_control_an_ordinary_new_file_is_not_owner_only() {
    // If this passed vacuously the checks below would prove nothing: a plain file in the same kind
    // of directory inherits more than one entry, or an inherited one.
    let dir = scratch("control");
    let plain = dir.join("plain.txt");
    std::fs::write(&plain, b"x").unwrap();
    let e = entries(&plain);
    assert!(
        e.len() > 1 || e.iter().any(|x| x.contains("(i)")),
        "an ordinary file reads as {e:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn a_new_database_is_owner_only_and_still_works() {
    let dir = scratch("open");
    let file = dir.join("tenant.sqlite");
    let db = SqliteBackend::open(&file).await.unwrap();
    write_one(&db).await;
    only_me(&file, "open");
    drop(db);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn a_new_durable_database_and_its_wal_files_are_owner_only() {
    let dir = scratch("durable");
    let file = dir.join("tenant.sqlite");
    let db = SqliteBackend::open_durable(&file).await.unwrap();
    write_one(&db).await;
    for suffix in ["", "-wal", "-shm"] {
        let mut name = file.as_os_str().to_owned();
        name.push(suffix);
        let p = std::path::PathBuf::from(name);
        assert!(p.exists(), "{} should exist while open", p.display());
        only_me(&p, "open_durable");
    }
    drop(db);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn an_existing_file_keeps_its_acl() {
    let dir = scratch("existing");
    let file = dir.join("shared.sqlite");
    std::fs::write(&file, b"").unwrap();
    let before = entries(&file);
    let db = SqliteBackend::open(&file).await.unwrap();
    write_one(&db).await;
    assert_eq!(
        entries(&file),
        before,
        "an operator's chosen ACL is not changed"
    );
    drop(db);
    let _ = std::fs::remove_dir_all(&dir);
}
