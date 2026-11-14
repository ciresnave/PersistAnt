// SPDX-License-Identifier: MIT OR Apache-2.0
//! `TestDb`: a fresh PostgreSQL database per test, in place of `#[sqlx::test]` (ROADMAP row 29).
//!
//! Needs `PERSISTANT_TEST_DATABASE_URL` and a role with `CREATEDB`. Without the variable these tests
//! print a note and pass, **unless** `PERSISTANT_REQUIRE_DATABASE=1` (set by the CI job that provides a
//! server).

use persistant_relational::{Migration, RelationalBackend, Value};
use persistant_sqlx::{Cleanup, PgBackend, TestDb, TestDbOptions};
use std::time::Duration;

async fn admin() -> Option<PgBackend> {
    match std::env::var("PERSISTANT_TEST_DATABASE_URL") {
        Ok(url) => Some(PgBackend::connect(&url).await.expect("connect")),
        Err(_) => {
            assert!(
                std::env::var("PERSISTANT_REQUIRE_DATABASE").as_deref() != Ok("1"),
                "PERSISTANT_REQUIRE_DATABASE=1 but PERSISTANT_TEST_DATABASE_URL is not set"
            );
            eprintln!("skipped: PERSISTANT_TEST_DATABASE_URL is not set");
            None
        }
    }
}

async fn exists(admin: &PgBackend, name: &str) -> bool {
    let rows = admin
        .query(
            "SELECT COUNT(*) FROM pg_database WHERE datname = $1",
            &[Value::Text(name.to_string())],
        )
        .await
        .unwrap();
    rows[0].int(0) == Some(1)
}

/// Two databases at once do not see each other's tables or rows, though the tables share a name.
#[tokio::test]
async fn each_test_database_is_its_own() {
    let Some(admin) = admin().await else { return };
    let (a, b) = (
        TestDb::create(&admin).await.unwrap(),
        TestDb::create(&admin).await.unwrap(),
    );
    assert_ne!(a.name(), b.name());
    for (db, n) in [(&a, 1_i64), (&b, 2)] {
        db.backend()
            .execute("CREATE TABLE t (n BIGINT NOT NULL)", &[])
            .await
            .unwrap();
        db.backend()
            .execute("INSERT INTO t VALUES ($1)", &[Value::Int(n)])
            .await
            .unwrap();
    }
    for (db, n) in [(&a, 1_i64), (&b, 2)] {
        let rows = db.backend().query("SELECT n FROM t", &[]).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].int(0), Some(n));
    }
    a.drop_database().await.unwrap();
    b.drop_database().await.unwrap();
}

/// The migrations a test needs are applied to its database and to nothing else.
#[tokio::test]
async fn migrations_apply_to_the_test_database_only() {
    let Some(admin) = admin().await else { return };
    let db = TestDb::create(&admin).await.unwrap();
    let report = db
        .migrate(&[Migration::new(
            1,
            "things",
            "CREATE TABLE things (id BIGINT PRIMARY KEY);",
        )])
        .await
        .unwrap();
    assert_eq!(report.applied.len(), 1);
    db.backend()
        .execute("INSERT INTO things VALUES (1)", &[])
        .await
        .unwrap();
    let elsewhere = admin
        .query("SELECT to_regclass('things') IS NULL", &[])
        .await
        .unwrap();
    assert_eq!(elsewhere[0].values, vec![Value::Bool(true)]);
    db.drop_database().await.unwrap();
}

/// `drop_database` removes it, even with a connection the test left open.
#[tokio::test]
async fn drop_database_removes_it_even_with_a_connection_left_open() {
    let Some(admin) = admin().await else { return };
    let db = TestDb::create(&admin).await.unwrap();
    let name = db.name().to_string();
    let _held = db.backend().pool().acquire().await.unwrap();
    assert!(exists(&admin, &name).await);
    db.drop_database().await.unwrap();
    assert!(!exists(&admin, &name).await);
}

/// A `TestDb` that goes out of scope (a test that panicked, say) is dropped too, on a single-threaded
/// runtime as well: the cleanup must not wait on the runtime it is blocking.
#[tokio::test(flavor = "current_thread")]
async fn a_test_database_that_goes_out_of_scope_is_dropped_on_a_current_thread_runtime() {
    let Some(admin) = admin().await else { return };
    let name = {
        let db = TestDb::create(&admin).await.unwrap();
        db.backend()
            .execute("CREATE TABLE t (n BIGINT)", &[])
            .await
            .unwrap();
        assert!(exists(&admin, db.name()).await);
        db.name().to_string()
    };
    assert!(!exists(&admin, &name).await, "{name} outlived its TestDb");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_test_database_that_goes_out_of_scope_is_dropped_on_a_multi_thread_runtime() {
    let Some(admin) = admin().await else { return };
    let name = {
        let db = TestDb::create(&admin).await.unwrap();
        db.name().to_string()
    };
    assert!(!exists(&admin, &name).await, "{name} outlived its TestDb");
}

/// `Cleanup::Defer` leaves the database when the handle goes away; `drop_database` still drops it.
#[tokio::test]
async fn a_deferred_database_outlives_its_handle_until_it_is_dropped_by_name() {
    let Some(admin) = admin().await else { return };
    let opts = TestDbOptions::default().cleanup(Cleanup::Defer);
    let db = TestDb::create_with(&admin, opts).await.unwrap();
    let name = db.name().to_string();
    drop(db);
    assert!(exists(&admin, &name).await, "Defer leaves the database");
    // The control: the default mode drops it with the handle.
    let db = TestDb::create_with(&admin, TestDbOptions::default())
        .await
        .unwrap();
    let dropped = db.name().to_string();
    drop(db);
    assert!(!exists(&admin, &dropped).await, "Drop removes it");
    // An explicit drop_database drops even a deferred one.
    let db = TestDb::create_with(&admin, opts).await.unwrap();
    let explicit = db.name().to_string();
    db.drop_database().await.unwrap();
    assert!(!exists(&admin, &explicit).await);
    // Clean up the first (a sweep with a zero bound would also hit other tests' live databases).
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "DROP DATABASE IF EXISTS {name} WITH (FORCE)"
    )))
    .execute(admin.pool())
    .await
    .unwrap();
}

/// `sweep` drops only the old databases this module names, and no younger one.
#[tokio::test]
async fn sweep_drops_only_old_databases_made_by_testdb() {
    let Some(admin) = admin().await else { return };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let two_hours = 2 * 3600 * 1_000_000_000_u128;
    // Unique per run: this process id, so a parallel run's leftovers are not confused with these.
    let pid = std::process::id();
    let old = format!("persistant_test_{pid}_{}_0", now - two_hours);
    let young = format!("persistant_test_{pid}_{now}_1");
    let decoys = [
        format!("persistant_test_notmine_{pid}"),
        format!("persistant_test_{pid}_abc_2"),
    ];
    let all: Vec<&String> = [&old, &young].into_iter().chain(decoys.iter()).collect();
    for n in &all {
        sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {n}")))
            .execute(admin.pool())
            .await
            .unwrap();
    }
    let n = TestDb::sweep(&admin, Duration::from_secs(3600))
        .await
        .unwrap();
    assert!(n >= 1, "at least the old one went, got {n}");
    assert!(!exists(&admin, &old).await, "old and ours: dropped");
    assert!(exists(&admin, &young).await, "younger than the bound: kept");
    for d in &decoys {
        assert!(exists(&admin, d).await, "{d} is not a TestDb name: kept");
    }
    for n in all {
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "DROP DATABASE IF EXISTS {n} WITH (FORCE)"
        )))
        .execute(admin.pool())
        .await
        .unwrap();
    }
}

/// Migrations that no other test (or earlier run) has used, so the template is built by this test.
fn unique_migrations() -> Vec<Migration> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    vec![
        Migration::new(
            1,
            "items",
            format!("-- {nanos}\nCREATE TABLE items (id BIGINT PRIMARY KEY, label TEXT NOT NULL)"),
        ),
        Migration::new(
            2,
            "seed",
            "INSERT INTO items VALUES (1, 'from the template')",
        ),
    ]
}

/// A template-made database has the migrated schema and data, and is independent of its siblings.
#[tokio::test]
async fn a_database_made_from_a_template_has_the_schema_and_is_its_own() {
    let Some(admin) = admin().await else { return };
    let migrations = unique_migrations();
    let template = TestDb::template(&admin, &migrations).await.unwrap();
    assert!(exists(&admin, template.name()).await);
    let (a, b) = (
        TestDb::create_from(&template).await.unwrap(),
        TestDb::create_from(&template).await.unwrap(),
    );
    assert_ne!(a.name(), b.name());
    for db in [&a, &b] {
        let rows = db
            .backend()
            .query("SELECT label FROM items", &[])
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
    }
    // independent: a write in one is not in the other, and not in the template
    a.backend()
        .execute("INSERT INTO items VALUES (2, 'only in a')", &[])
        .await
        .unwrap();
    let in_b = b
        .backend()
        .query("SELECT id FROM items", &[])
        .await
        .unwrap();
    assert_eq!(in_b.len(), 1);
    let c = TestDb::create_from(&template).await.unwrap();
    let in_c = c
        .backend()
        .query("SELECT id FROM items", &[])
        .await
        .unwrap();
    assert_eq!(
        in_c.len(),
        1,
        "a write to a copy must not reach the template"
    );
    // control: an empty TestDb has no such table
    let plain = TestDb::create(&admin).await.unwrap();
    assert!(
        plain
            .backend()
            .query("SELECT id FROM items", &[])
            .await
            .is_err()
    );
    for db in [a, b, c, plain] {
        db.drop_database().await.unwrap();
    }
    template.drop_database().await.unwrap();
}

/// The same migrations find the same template, different ones a different template, and many callers at
/// once build it once and all get a copy.
#[tokio::test]
async fn a_template_is_named_for_its_migrations_and_built_once_under_concurrency() {
    let Some(admin) = admin().await else { return };
    let migrations = unique_migrations();
    let first = TestDb::template(&admin, &migrations).await.unwrap();
    let again = TestDb::template(&admin, &migrations).await.unwrap();
    assert_eq!(first.name(), again.name());
    let mut changed = migrations.clone();
    changed[1] = Migration::new(2, "seed", "INSERT INTO items VALUES (1, 'different')");
    let other = TestDb::template(&admin, &changed).await.unwrap();
    assert_ne!(first.name(), other.name());
    first.drop_database().await.unwrap();
    other.drop_database().await.unwrap();

    // now gone: eight callers race to build it
    let fresh = unique_migrations();
    let mut tasks = Vec::new();
    for _ in 0..8 {
        let (admin_url, fresh) = (
            std::env::var("PERSISTANT_TEST_DATABASE_URL").unwrap(),
            fresh.clone(),
        );
        tasks.push(tokio::spawn(async move {
            let admin = PgBackend::connect(&admin_url).await.unwrap();
            let t = TestDb::template(&admin, &fresh).await.unwrap();
            let db = TestDb::create_from(&t).await.unwrap();
            let n = db
                .backend()
                .query("SELECT id FROM items", &[])
                .await
                .unwrap()
                .len();
            let name = t.name().to_string();
            db.drop_database().await.unwrap();
            (name, n)
        }));
    }
    let mut names = std::collections::BTreeSet::new();
    for t in tasks {
        let (name, n) = t.await.unwrap();
        assert_eq!(n, 1);
        names.insert(name);
    }
    assert_eq!(names.len(), 1, "one template for one list of migrations");
    let name = names.into_iter().next().unwrap();
    let rows = admin
        .query(
            "SELECT COUNT(*) FROM pg_database WHERE datname = $1",
            &[Value::Text(name.clone())],
        )
        .await
        .unwrap();
    assert_eq!(rows[0].int(0), Some(1));
    // clean up through a handle for the same migrations
    TestDb::template(&admin, &fresh)
        .await
        .unwrap()
        .drop_database()
        .await
        .unwrap();
}

/// A migration that fails leaves no template to copy and reports the failure.
#[tokio::test]
async fn a_failing_migration_leaves_no_template() {
    let Some(admin) = admin().await else { return };
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let bad = vec![Migration::new(
        1,
        "broken",
        format!("-- {nanos}\nCREATE TABLE ("),
    )];
    assert!(TestDb::template(&admin, &bad).await.is_err());
    // a second try fails the same way: nothing half-built was left behind as if it were done
    assert!(TestDb::template(&admin, &bad).await.is_err());
}
