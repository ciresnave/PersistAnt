// SPDX-License-Identifier: MIT OR Apache-2.0
//! The `persistant-migrate` binary run for real against PostgreSQL and MySQL/MariaDB servers: the
//! same scenario as the SQLite tests, plus `verify-fresh`, which must leave the target alone and
//! leave no scratch schema or database behind.
//!
//! PostgreSQL needs `PERSISTANT_TEST_DATABASE_URL`, MySQL/MariaDB `PERSISTANT_TEST_MYSQL_URL`. Without
//! one a test prints a note and passes **unless** `PERSISTANT_REQUIRE_DATABASE=1` /
//! `PERSISTANT_REQUIRE_MYSQL=1` (set by the CI jobs that provide the server). Each run works in its
//! own schema (PostgreSQL) or database (MySQL), dropped afterwards.

use persistant_relational::RelationalBackend;
use persistant_sqlx::{MysqlBackend, PgBackend};
use std::path::Path;
use std::process::{Command, Output};

fn write(dir: &Path, name: &str, sql: &str) {
    std::fs::write(dir.join(name), sql).unwrap();
}

fn url_from(var: &str, require: &str) -> Option<String> {
    if let Ok(u) = std::env::var(var) {
        return Some(u);
    }
    assert!(
        std::env::var(require).as_deref() != Ok("1"),
        "{require}=1 but {var} is not set"
    );
    eprintln!("skipped: {var} is not set");
    None
}

fn run(args: &[&str], url: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_persistant-migrate"))
        .args(args)
        .env("PERSISTANT_DATABASE_URL", url)
        .output()
        .unwrap()
}

fn out(o: &Output) -> String {
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

/// Everything the CLI promises, against the database `url` points at. `leftovers` counts scratch
/// objects `verify-fresh` may have left on the server; `has_things` says whether the target has the
/// `things` table.
async fn scenario<L, T>(url: &str, leftovers: L, has_things: T)
where
    L: AsyncFn() -> i64,
    T: AsyncFn() -> bool,
{
    let t = tempfile::tempdir().unwrap();
    let m = migrations(t.path());
    let dir = m.to_str().unwrap();

    // The target is named on stderr, without a password.
    let st = run(&["status", "--dir", dir], url);
    assert_eq!(st.status.code(), Some(0), "{st:?}");
    assert_eq!(out(&st).matches("pending").count(), 2);
    let err = String::from_utf8_lossy(&st.stderr);
    assert!(err.contains("status on "), "{err}");
    assert!(!err.contains("persistant-test"), "password leaked: {err}");

    // verify-fresh: succeeds, applies nothing to the target, leaves no scratch object.
    let before = leftovers().await;
    let vf = run(&["verify-fresh", "--dir", dir], url);
    assert_eq!(vf.status.code(), Some(0), "{vf:?}");
    assert!(out(&vf).contains("2 migrations"), "{}", out(&vf));
    assert_eq!(
        leftovers().await,
        before,
        "verify-fresh left a scratch object"
    );
    assert!(!has_things().await, "verify-fresh touched the target");
    assert_eq!(
        out(&run(&["status", "--dir", dir], url))
            .matches("pending")
            .count(),
        2
    );

    // up applies, a second up is a no-op, status agrees.
    let up = run(&["up", "--dir", dir], url);
    assert_eq!(up.status.code(), Some(0), "{up:?}");
    assert!(out(&up).contains("applied [1, 2]"), "{}", out(&up));
    assert!(has_things().await, "up did not create the table");
    let again = run(&["up", "--dir", dir], url);
    assert!(out(&again).contains("already applied [1, 2]"));
    let after = run(&["status", "--dir", dir], url);
    assert_eq!(after.status.code(), Some(0));
    assert_eq!(out(&after).matches("applied").count(), 2);

    // A changed migration is exit 3, for status and for up.
    write(
        &m,
        "2_more.up.sql",
        "ALTER TABLE things ADD COLUMN other TEXT;",
    );
    let changed = run(&["status", "--dir", dir], url);
    assert_eq!(changed.status.code(), Some(3), "{changed:?}");
    assert!(out(&changed).contains("CHANGED"));
    assert_eq!(run(&["up", "--dir", dir], url).status.code(), Some(3));

    // A database newer than the directory is exit 4.
    std::fs::remove_file(m.join("2_more.up.sql")).unwrap();
    let newer = run(&["status", "--dir", dir], url);
    assert_eq!(newer.status.code(), Some(4), "{newer:?}");
    assert!(out(&newer).contains("UNKNOWN"));

    // A set that cannot install on an empty database fails verify-fresh (6) and still leaves nothing.
    write(
        &m,
        "2_more.up.sql",
        "ALTER TABLE no_such_table ADD COLUMN n TEXT;",
    );
    let before = leftovers().await;
    let bad = run(&["verify-fresh", "--dir", dir], url);
    assert_eq!(bad.status.code(), Some(6), "{bad:?}");
    assert_eq!(
        leftovers().await,
        before,
        "a failed verify-fresh left a scratch object"
    );
}

#[tokio::test]
async fn the_cli_on_postgres() {
    let Some(base) = url_from(
        "PERSISTANT_TEST_DATABASE_URL",
        "PERSISTANT_REQUIRE_DATABASE",
    ) else {
        return;
    };
    let schema = format!("persistant_cli_{}", std::process::id());
    let admin = PgBackend::connect(&base).await.expect("connect");
    admin
        .execute(&format!("DROP SCHEMA IF EXISTS {schema} CASCADE"), &[])
        .await
        .unwrap();
    admin
        .execute(&format!("CREATE SCHEMA {schema}"), &[])
        .await
        .unwrap();
    let sep = if base.contains('?') { '&' } else { '?' };
    let url = format!("{base}{sep}options=-c%20search_path%3D{schema}");
    scenario(
        &url,
        async || {
            let rows = admin
                .query(
                    "SELECT COUNT(*) FROM pg_namespace WHERE nspname LIKE 'persistant_fresh_%'",
                    &[],
                )
                .await
                .unwrap();
            rows[0].int(0).unwrap()
        },
        async || {
            let rows = admin
                .query(
                    &format!(
                        "SELECT COUNT(*) FROM information_schema.tables \
                         WHERE table_schema = '{schema}' AND table_name = 'things'"
                    ),
                    &[],
                )
                .await
                .unwrap();
            rows[0].int(0).unwrap() == 1
        },
    )
    .await;
    admin
        .execute(&format!("DROP SCHEMA {schema} CASCADE"), &[])
        .await
        .unwrap();
}

#[tokio::test]
async fn the_cli_on_mysql() {
    let Some(base) = url_from("PERSISTANT_TEST_MYSQL_URL", "PERSISTANT_REQUIRE_MYSQL") else {
        return;
    };
    let db = format!("persistant_cli_{}", std::process::id());
    let admin = MysqlBackend::connect(&base).await.expect("connect");
    admin
        .execute(&format!("DROP DATABASE IF EXISTS {db}"), &[])
        .await
        .unwrap();
    admin
        .execute(&format!("CREATE DATABASE {db}"), &[])
        .await
        .unwrap();
    // The same server and credentials, a different database.
    let (head, _) = base.rsplit_once('/').expect("a database in the URL");
    let url = format!("{head}/{db}");
    scenario(
        &url,
        async || {
            let rows = admin
                .query(
                    "SELECT COUNT(*) FROM information_schema.schemata \
                     WHERE schema_name LIKE 'persistant\\_fresh\\_%'",
                    &[],
                )
                .await
                .unwrap();
            rows[0].int(0).unwrap()
        },
        async || {
            let rows = admin
                .query(
                    &format!(
                        "SELECT COUNT(*) FROM information_schema.tables \
                         WHERE table_schema = '{db}' AND table_name = 'things'"
                    ),
                    &[],
                )
                .await
                .unwrap();
            rows[0].int(0).unwrap() == 1
        },
    )
    .await;
    admin
        .execute(&format!("DROP DATABASE {db}"), &[])
        .await
        .unwrap();
}
