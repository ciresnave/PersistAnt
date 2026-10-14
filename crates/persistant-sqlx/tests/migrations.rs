// SPDX-License-Identifier: MIT OR Apache-2.0
//! Migrations against a real PostgreSQL. Each test works in its own schema (set through the
//! connection's `search_path`), so the ledger `_sqlx_migrations` is the test's own.
//!
//! Needs `PERSISTANT_TEST_DATABASE_URL`; skipped without it unless `PERSISTANT_REQUIRE_DATABASE=1`.

use persistant_relational::{RelError, RelationalBackend, Value};
use persistant_sqlx::{MigrateError, Migration, PgBackend};
use sha2::{Digest, Sha384};

async fn isolated(name: &str) -> Option<PgBackend> {
    let Ok(url) = std::env::var("PERSISTANT_TEST_DATABASE_URL") else {
        assert!(
            std::env::var("PERSISTANT_REQUIRE_DATABASE").as_deref() != Ok("1"),
            "PERSISTANT_REQUIRE_DATABASE=1 but PERSISTANT_TEST_DATABASE_URL is not set"
        );
        eprintln!("skipped: PERSISTANT_TEST_DATABASE_URL is not set");
        return None;
    };
    let schema = format!("persistant_mig_{name}");
    let admin = PgBackend::connect(&url).await.expect("connect");
    admin
        .execute(&format!("DROP SCHEMA IF EXISTS {schema} CASCADE"), &[])
        .await
        .unwrap();
    admin
        .execute(&format!("CREATE SCHEMA {schema}"), &[])
        .await
        .unwrap();
    let sep = if url.contains('?') { '&' } else { '?' };
    let scoped = format!("{url}{sep}options=-c%20search_path%3D{schema}");
    Some(PgBackend::connect(&scoped).await.expect("connect scoped"))
}

fn two() -> Vec<Migration> {
    vec![
        Migration::new(
            1,
            "create things",
            "CREATE TABLE things (id INT PRIMARY KEY, n TEXT);",
        ),
        Migration::new(2, "add index", "CREATE INDEX things_n ON things (n);"),
    ]
}

#[tokio::test]
async fn applies_in_order_then_a_second_run_applies_nothing() {
    let Some(db) = isolated("applies").await else {
        return;
    };
    let r = db.migrate(&two()).await.unwrap();
    assert_eq!(r.applied, vec![1, 2]);
    assert!(r.already_applied.is_empty());
    db.execute("INSERT INTO things VALUES (1, 'x')", &[])
        .await
        .unwrap();
    let again = db.migrate(&two()).await.unwrap();
    assert!(again.applied.is_empty());
    assert_eq!(again.already_applied, vec![1, 2]);
    assert_eq!(db.applied_versions().await.unwrap(), vec![1, 2]);
}

#[tokio::test]
async fn a_new_migration_is_added_to_an_existing_ledger() {
    let Some(db) = isolated("adds").await else {
        return;
    };
    db.migrate(&two()).await.unwrap();
    let mut three = two();
    three.push(Migration::new(
        3,
        "add column",
        "ALTER TABLE things ADD COLUMN extra INT;",
    ));
    let r = db.migrate(&three).await.unwrap();
    assert_eq!(r.applied, vec![3]);
    assert_eq!(r.already_applied, vec![1, 2]);
}

#[tokio::test]
async fn an_edited_applied_migration_is_refused_by_checksum() {
    let Some(db) = isolated("edited").await else {
        return;
    };
    db.migrate(&two()).await.unwrap();
    let mut edited = two();
    edited[0].sql = "CREATE TABLE things (id INT PRIMARY KEY, n TEXT, sneaky INT);".to_string();
    let e = db.migrate(&edited).await.unwrap_err();
    assert_eq!(e, MigrateError::ChecksumMismatch { version: 1 });
}

#[tokio::test]
async fn a_database_newer_than_the_program_is_refused() {
    let Some(db) = isolated("newer").await else {
        return;
    };
    db.migrate(&two()).await.unwrap();
    // A program that only knows version 1 meets a database that has applied 1 and 2.
    let e = db.migrate(&two()[..1]).await.unwrap_err();
    assert_eq!(e, MigrateError::NewerThanProgram { version: 2 });
}

#[tokio::test]
async fn a_failing_migration_rolls_back_and_names_its_version_and_class() {
    let Some(db) = isolated("fails").await else {
        return;
    };
    let migs = vec![
        Migration::new(1, "ok", "CREATE TABLE ok_table (id INT PRIMARY KEY);"),
        Migration::new(
            2,
            "half then break",
            "CREATE TABLE half (id INT); INSERT INTO ok_table VALUES (1); INSERT INTO ok_table VALUES (1);",
        ),
    ];
    let e = db.migrate(&migs).await.unwrap_err();
    assert!(
        matches!(
            &e,
            MigrateError::Failed {
                version: 2,
                error: RelError::UniqueViolation { .. }
            }
        ),
        "{e:?}"
    );
    assert_eq!(db.applied_versions().await.unwrap(), vec![1]);
    let half = db
        .query("SELECT to_regclass('half')::text", &[])
        .await
        .unwrap();
    assert_eq!(
        half[0].values[0],
        Value::Null,
        "the failed migration's DDL was rolled back"
    );
}

#[tokio::test]
async fn the_ledger_is_sqlxs_own_shape_with_sha384_checksums() {
    let Some(db) = isolated("shape").await else {
        return;
    };
    let migs = two();
    db.migrate(&migs).await.unwrap();
    let cols = db
        .query(
            "SELECT column_name FROM information_schema.columns \
             WHERE table_schema = current_schema() AND table_name = '_sqlx_migrations' \
             ORDER BY column_name",
            &[],
        )
        .await
        .unwrap();
    let names: Vec<&str> = cols.iter().filter_map(|r| r.text(0)).collect();
    assert_eq!(
        names,
        vec![
            "checksum",
            "description",
            "execution_time",
            "installed_on",
            "success",
            "version"
        ]
    );
    let rows = db
        .query(
            "SELECT version, checksum FROM _sqlx_migrations ORDER BY version",
            &[],
        )
        .await
        .unwrap();
    for (row, m) in rows.iter().zip(&migs) {
        let want = Sha384::digest(m.sql.as_bytes());
        assert_eq!(row.int(0), Some(m.version));
        assert_eq!(
            row.bytes(1),
            Some(&want[..]),
            "checksum is SHA-384 of the migration text, as sqlx writes it"
        );
    }
}
