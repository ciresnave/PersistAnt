// SPDX-License-Identifier: MIT OR Apache-2.0
//! Migrations against a real PostgreSQL. Each test works in its own schema (set through the
//! connection's `search_path`), so the ledger `_sqlx_migrations` is the test's own.
//!
//! Needs `PERSISTANT_TEST_DATABASE_URL`; skipped without it unless `PERSISTANT_REQUIRE_DATABASE=1`.

use persistant_relational::{RelError, RelationalBackend, Value};
use persistant_sqlx::{FromSqlxError, MigrateError, Migration, PgBackend, migrations_from_sqlx};
use sha2::{Digest, Sha384};
use sqlx::SqlSafeStr;
use sqlx::migrate::{Migration as SqlxMigration, MigrationType, Migrator};

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

fn sqlx_mig(v: i64, kind: MigrationType, sql: &'static str, no_tx: bool) -> SqlxMigration {
    SqlxMigration::new(
        v,
        format!("m{v}").into(),
        kind,
        sqlx::AssertSqlSafe(sql).into_sql_str(),
        no_tx,
    )
}

#[test]
fn a_migrator_maps_to_migrations_and_down_halves_are_dropped() {
    let m = Migrator::with_migrations(vec![
        sqlx_mig(1, MigrationType::Simple, "CREATE TABLE a (id INT);", false),
        sqlx_mig(
            2,
            MigrationType::ReversibleUp,
            "CREATE TABLE b (id INT);",
            false,
        ),
        sqlx_mig(2, MigrationType::ReversibleDown, "DROP TABLE b;", false),
    ]);
    let got = migrations_from_sqlx(&m).unwrap();
    assert_eq!(
        got,
        vec![
            Migration::new(1, "m1", "CREATE TABLE a (id INT);"),
            Migration::new(2, "m2", "CREATE TABLE b (id INT);"),
        ]
    );
}

#[test]
fn what_sqlx_would_do_differently_is_refused() {
    let mut m =
        Migrator::with_migrations(vec![sqlx_mig(1, MigrationType::Simple, "SELECT 1;", true)]);
    assert_eq!(
        migrations_from_sqlx(&m),
        Err(FromSqlxError::NoTransaction { version: 1 })
    );
    m = Migrator::with_migrations(vec![sqlx_mig(1, MigrationType::Simple, "SELECT 1;", false)]);
    m.dangerous_set_table_name("other_ledger");
    assert!(matches!(
        migrations_from_sqlx(&m),
        Err(FromSqlxError::LedgerTable { .. })
    ));
    let mut m =
        Migrator::with_migrations(vec![sqlx_mig(1, MigrationType::Simple, "SELECT 1;", false)]);
    m.set_locking(false);
    assert_eq!(
        migrations_from_sqlx(&m),
        Err(FromSqlxError::Setting("locking = false"))
    );
    // Positive control: the same migrator, unchanged, is accepted.
    let ok =
        Migrator::with_migrations(vec![sqlx_mig(1, MigrationType::Simple, "SELECT 1;", false)]);
    assert!(migrations_from_sqlx(&ok).is_ok());
}

#[tokio::test]
async fn a_database_migrated_by_sqlx_is_accepted_and_the_other_way_round() {
    let Some(db) = isolated("from_sqlx").await else {
        return;
    };
    let sqlx_side = Migrator::with_migrations(vec![
        sqlx_mig(
            1,
            MigrationType::Simple,
            "CREATE TABLE sx (id INT PRIMARY KEY);",
            false,
        ),
        sqlx_mig(
            2,
            MigrationType::Simple,
            "CREATE INDEX sx_id ON sx (id);",
            false,
        ),
    ]);
    // sqlx's own runner writes the ledger; this crate then sees both as already applied.
    sqlx_side.run(db.pool()).await.unwrap();
    let ours = migrations_from_sqlx(&sqlx_side).unwrap();
    let r = db.migrate(&ours).await.unwrap();
    assert!(r.applied.is_empty());
    assert_eq!(r.already_applied, vec![1, 2]);
    // And the checksum this crate would write is the one sqlx wrote.
    let stored: Vec<Vec<u8>> =
        sqlx::query_scalar("SELECT checksum FROM _sqlx_migrations ORDER BY version")
            .fetch_all(db.pool())
            .await
            .unwrap();
    let want: Vec<Vec<u8>> = sqlx_side.iter().map(|m| m.checksum.to_vec()).collect();
    assert_eq!(stored, want);
}

/// The scratch-schema count is server-wide, so the two tests that read it run one at a time.
static FRESH: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn schemas_named_fresh(db: &PgBackend) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM pg_namespace WHERE nspname LIKE 'persistant_fresh_%'")
        .fetch_one(db.pool())
        .await
        .unwrap()
}

#[tokio::test]
async fn verify_fresh_installs_on_an_empty_schema_and_leaves_nothing() {
    let _one = FRESH.lock().await;
    let Some(db) = isolated("fresh_ok").await else {
        return;
    };
    let before = schemas_named_fresh(&db).await;
    let r = db.verify_fresh(&two()).await.unwrap();
    assert_eq!(r.applied, vec![1, 2]);
    // Nothing in the target schema: no ledger, no table. Nothing left over in the scratch schema.
    assert!(db.applied_versions().await.unwrap().is_empty());
    assert!(db.execute("SELECT * FROM things", &[]).await.is_err());
    assert_eq!(schemas_named_fresh(&db).await, before);
    // Positive control: the same migrations do install for real.
    assert_eq!(db.migrate(&two()).await.unwrap().applied, vec![1, 2]);
}

#[tokio::test]
async fn verify_fresh_fails_for_a_migration_that_only_works_on_an_existing_database() {
    let _one = FRESH.lock().await;
    let Some(db) = isolated("fresh_bad").await else {
        return;
    };
    // The target already has the table; a fresh install must not, so the second migration fails.
    db.execute("CREATE TABLE legacy (id INT)", &[])
        .await
        .unwrap();
    let ms = vec![Migration::new(
        1,
        "uses legacy",
        "ALTER TABLE legacy ADD COLUMN n INT;",
    )];
    let before = schemas_named_fresh(&db).await;
    assert!(matches!(
        db.verify_fresh(&ms).await,
        Err(MigrateError::Failed { version: 1, .. })
    ));
    assert_eq!(schemas_named_fresh(&db).await, before);
    // Positive control: applied for real against the existing table, it succeeds.
    assert_eq!(db.migrate(&ms).await.unwrap().applied, vec![1]);
}

#[tokio::test]
async fn the_report_carries_each_migrations_checksum() {
    let Some(db) = isolated("report_checksums").await else {
        return;
    };
    let want: Vec<(i64, Vec<u8>)> = two()
        .iter()
        .map(|m| (m.version, Sha384::digest(m.sql.as_bytes()).to_vec()))
        .collect();
    let first = db.migrate(&two()).await.unwrap();
    assert_eq!(first.checksums, want);
    let again = db.migrate(&two()).await.unwrap();
    assert!(again.applied.is_empty());
    assert_eq!(again.checksums, want);
}

#[tokio::test]
async fn applied_checksums_reads_the_ledger_without_applying_anything() {
    let Some(db) = isolated("applied_checksums").await else {
        return;
    };
    assert!(db.applied_checksums().await.unwrap().is_empty());
    let report = db.migrate(&two()).await.unwrap();
    assert_eq!(db.applied_checksums().await.unwrap(), report.checksums);
}
