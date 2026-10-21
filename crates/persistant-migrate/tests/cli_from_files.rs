// SPDX-License-Identifier: MIT OR Apache-2.0
//! `persistant-migrate snapshot --from-files`: a snapshot read from the migration files alone, with no
//! database and no URL. Run through the real binary.

use persistant_relational::catalog;
use std::path::Path;
use std::process::{Command, Output};

fn write(dir: &Path, name: &str, sql: &str) {
    std::fs::write(dir.join(name), sql).unwrap();
}

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_persistant-migrate"))
        .args(args)
        .env_remove("PERSISTANT_DATABASE_URL")
        .output()
        .unwrap()
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn migrations(dir: &Path) -> std::path::PathBuf {
    let m = dir.join("migrations");
    std::fs::create_dir(&m).unwrap();
    write(
        &m,
        "1_things.up.sql",
        "CREATE TABLE things (id INTEGER PRIMARY KEY, name TEXT NOT NULL);",
    );
    write(&m, "2_more.up.sql", "ALTER TABLE things ADD COLUMN n TEXT;");
    m
}

#[test]
fn from_files_writes_checks_and_goes_stale_without_a_database() {
    let t = tempfile::tempdir().unwrap();
    let m = migrations(t.path());
    let dir = m.to_str().unwrap();
    let file = t.path().join("schema.json");
    let f = file.to_str().unwrap();
    let base = [
        "snapshot",
        "--from-files",
        "--dir",
        dir,
        "--dialect",
        "sqlite",
    ];

    let w = run(&[&base[..], &["--out", f]].concat());
    assert_eq!(w.status.code(), Some(0), "{}", stderr(&w));
    assert!(
        stdout(&w).contains("1 tables, 2 migrations"),
        "{}",
        stdout(&w)
    );
    // No target line: nothing was connected to.
    assert!(!stderr(&w).contains(" on "), "{}", stderr(&w));
    let snap = catalog::parse(&std::fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!(snap.meta.get("source").map(String::as_str), Some("ddl"));
    assert_eq!(snap.meta.get("dialect").map(String::as_str), Some("sqlite"));
    let things = snap.schema.tables().next().unwrap();
    assert!(things.columns.contains_key("n"), "ALTER was applied");
    assert_eq!(things.columns["name"].nullable, Some(false));

    let c = run(&[&base[..], &["--out", f, "--check"]].concat());
    assert_eq!(c.status.code(), Some(0), "{}", stderr(&c));
    assert!(stdout(&c).contains("is current"), "{}", stdout(&c));

    // A new migration makes the committed file stale, and says which one.
    write(
        &m,
        "3_third.up.sql",
        "ALTER TABLE things ADD COLUMN m TEXT;",
    );
    let s = run(&[&base[..], &["--out", f, "--check"]].concat());
    assert_eq!(s.status.code(), Some(8), "{}", stderr(&s));
    assert!(
        stderr(&s).contains("migration 3 is not in the snapshot"),
        "{}",
        stderr(&s)
    );
}

#[test]
fn from_files_refuses_a_url_a_missing_dialect_and_scratch() {
    let t = tempfile::tempdir().unwrap();
    let m = migrations(t.path());
    let dir = m.to_str().unwrap();
    let out = t.path().join("s.json");
    let f = out.to_str().unwrap();
    for args in [
        vec!["snapshot", "--from-files", "--dir", dir, "--out", f],
        vec![
            "snapshot",
            "--from-files",
            "--dir",
            dir,
            "--dialect",
            "sqlite",
            "--url",
            "sqlite:x.db",
        ],
        vec![
            "snapshot",
            "--from-files",
            "--scratch",
            "--dir",
            dir,
            "--dialect",
            "sqlite",
        ],
        vec!["up", "--from-files", "--dir", dir, "--dialect", "sqlite"],
    ] {
        let o = run(&args);
        assert_eq!(o.status.code(), Some(2), "{args:?}: {}", stderr(&o));
    }
    assert!(!out.exists(), "nothing was written");
    let bad = run(&[
        "snapshot",
        "--from-files",
        "--dir",
        dir,
        "--dialect",
        "oracle",
        "--out",
        f,
    ]);
    assert_eq!(bad.status.code(), Some(2), "{}", stderr(&bad));
}
