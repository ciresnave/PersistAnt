// SPDX-License-Identifier: MIT OR Apache-2.0
//! The `persistant-migrate` binary, run for real against an SQLite file: what it prints and the exit
//! code each situation gets. The same code paths serve PostgreSQL and MySQL; those are covered by
//! `persistant-sqlx`'s own migration tests.

use persistant_migrate::{State, checksum, status};
use persistant_relational::Migration;
use std::path::Path;
use std::process::{Command, Output};

fn write(dir: &Path, name: &str, sql: &str) {
    std::fs::write(dir.join(name), sql).unwrap();
}

fn run(args: &[&str], db: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_persistant-migrate"))
        .args(args)
        .arg("--url")
        .arg(format!("sqlite:{}", db.display()))
        .env_remove("PERSISTANT_DATABASE_URL")
        .output()
        .unwrap()
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn migrations(dir: &Path) -> std::path::PathBuf {
    let m = dir.join("migrations");
    std::fs::create_dir(&m).unwrap();
    write(
        &m,
        "1_things.up.sql",
        "CREATE TABLE things (id INTEGER PRIMARY KEY);",
    );
    write(&m, "1_things.down.sql", "DROP TABLE things;");
    write(&m, "2_more.up.sql", "ALTER TABLE things ADD COLUMN n TEXT;");
    m
}

#[test]
fn up_then_status_then_a_changed_migration_is_exit_3() {
    let t = tempfile::tempdir().unwrap();
    let m = migrations(t.path());
    let db = t.path().join("a.db");
    let dir = m.to_str().unwrap();

    let before = run(&["status", "--dir", dir], &db);
    assert_eq!(before.status.code(), Some(0), "{before:?}");
    assert_eq!(stdout(&before).matches("pending").count(), 2);

    let up = run(&["up", "--dir", dir], &db);
    assert_eq!(up.status.code(), Some(0), "{up:?}");
    assert!(stdout(&up).contains("applied [1, 2]"), "{}", stdout(&up));

    let after = run(&["status", "--dir", dir], &db);
    assert_eq!(after.status.code(), Some(0));
    assert_eq!(stdout(&after).matches("applied").count(), 2);

    // The down file was read, never run: the table is still there, so a second `up` is a no-op.
    let again = run(&["up", "--dir", dir], &db);
    assert!(stdout(&again).contains("already applied [1, 2]"));

    write(
        &m,
        "2_more.up.sql",
        "ALTER TABLE things ADD COLUMN other TEXT;",
    );
    let changed = run(&["status", "--dir", dir], &db);
    assert_eq!(changed.status.code(), Some(3), "{changed:?}");
    assert!(stdout(&changed).contains("CHANGED"));
    let refused = run(&["up", "--dir", dir], &db);
    assert_eq!(refused.status.code(), Some(3), "{refused:?}");
}

#[test]
fn a_database_newer_than_the_directory_is_exit_4() {
    let t = tempfile::tempdir().unwrap();
    let m = migrations(t.path());
    let db = t.path().join("b.db");
    assert_eq!(
        run(&["up", "--dir", m.to_str().unwrap()], &db)
            .status
            .code(),
        Some(0)
    );
    std::fs::remove_file(m.join("2_more.up.sql")).unwrap();
    let o = run(&["status", "--dir", m.to_str().unwrap()], &db);
    assert_eq!(o.status.code(), Some(4), "{o:?}");
    assert!(stdout(&o).contains("UNKNOWN"));
}

#[test]
fn a_down_file_with_no_up_is_refused() {
    let t = tempfile::tempdir().unwrap();
    let m = migrations(t.path());
    write(&m, "3_orphan.down.sql", "SELECT 1;");
    let o = run(
        &["up", "--dir", m.to_str().unwrap()],
        &t.path().join("c.db"),
    );
    assert_eq!(o.status.code(), Some(7), "{o:?}");
    assert!(String::from_utf8_lossy(&o.stderr).contains("3 has a .down.sql but no .up.sql"));
}

#[test]
fn verify_fresh_leaves_the_target_database_alone_and_catches_a_broken_set() {
    let t = tempfile::tempdir().unwrap();
    let m = migrations(t.path());
    let db = t.path().join("d.db");
    let ok = run(&["verify-fresh", "--dir", m.to_str().unwrap()], &db);
    assert_eq!(ok.status.code(), Some(0), "{ok:?}");
    // Nothing was applied to the real target.
    let st = run(&["status", "--dir", m.to_str().unwrap()], &db);
    assert_eq!(stdout(&st).matches("pending").count(), 2);

    write(
        &m,
        "2_more.up.sql",
        "ALTER TABLE no_such_table ADD COLUMN n TEXT;",
    );
    let bad = run(&["verify-fresh", "--dir", m.to_str().unwrap()], &db);
    assert_eq!(bad.status.code(), Some(6), "{bad:?}");
}

#[test]
fn usage_errors_are_exit_2() {
    let o = Command::new(env!("CARGO_BIN_EXE_persistant-migrate"))
        .arg("up")
        .env_remove("PERSISTANT_DATABASE_URL")
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(2));
}

#[test]
fn status_compares_text_checksums_and_names_unknown_versions() {
    let a = Migration::new(1, "a", "SELECT 1");
    let b = Migration::new(2, "b", "SELECT 2");
    let ledger = vec![
        (1, checksum(&a)),
        (2, checksum(&Migration::new(2, "b", "SELECT 3"))),
        (9, vec![0]),
    ];
    let states: Vec<State> = status(&[a, b], &ledger)
        .into_iter()
        .map(|l| l.state)
        .collect();
    assert_eq!(states, [State::Applied, State::Changed, State::Unknown]);
    let none = status(&[Migration::new(1, "a", "SELECT 1")], &[]);
    assert_eq!(none[0].state, State::Pending);
}

fn run_url(args: &[&str], url: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_persistant-migrate"))
        .args(args)
        .arg("--url")
        .arg(url)
        .env_remove("PERSISTANT_DATABASE_URL")
        .output()
        .unwrap()
}

#[test]
fn a_remote_target_is_refused_and_named_without_its_password() {
    let t = tempfile::tempdir().unwrap();
    let m = migrations(t.path());
    let dir = m.to_str().unwrap();
    // 192.0.2.1 is TEST-NET-1: never routable, and the guard must refuse before any connection.
    let o = run_url(
        &["up", "--dir", dir],
        "postgres://app:s3cretpw@192.0.2.1:5432/prod",
    );
    assert_eq!(o.status.code(), Some(9), "{o:?}");
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(
        err.contains("up on postgres://app@192.0.2.1:5432/prod"),
        "{err}"
    );
    assert!(!err.contains("s3cretpw"), "{err}");
    assert!(err.contains("--allow-remote"), "{err}");
    // Read-only commands are guarded too, and a host= override cannot smuggle a remote host in.
    let o = run_url(
        &["status", "--dir", dir],
        "postgres://app@localhost/prod?host=192.0.2.1",
    );
    assert_eq!(o.status.code(), Some(9), "{o:?}");
    // Positive control: with the flag the guard lets it through (and the connection then fails, 7).
    let o = run_url(
        &["status", "--dir", dir, "--allow-remote"],
        "mysql://app@192.0.2.1:1/prod",
    );
    assert_eq!(o.status.code(), Some(7), "{o:?}");
}

#[test]
fn a_local_target_is_printed_and_allowed() {
    let t = tempfile::tempdir().unwrap();
    let m = migrations(t.path());
    let db = t.path().join("g.db");
    let o = run(&["status", "--dir", m.to_str().unwrap()], &db);
    assert_eq!(o.status.code(), Some(0), "{o:?}");
    assert!(String::from_utf8_lossy(&o.stderr).contains("status on sqlite:"));
    for url in [
        "postgres://u:p@localhost/d",
        "mysql://u@127.0.0.1:3306/d",
        "postgres://u@[::1]/d",
        "postgres:///d?host=/var/run/postgresql",
    ] {
        assert!(persistant_migrate::describe_target(url).local, "{url}");
    }
    assert!(!persistant_migrate::describe_target("nonsense").local);
}
