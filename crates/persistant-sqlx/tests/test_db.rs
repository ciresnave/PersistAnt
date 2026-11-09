// SPDX-License-Identifier: MIT OR Apache-2.0
//! `TestDb`: a fresh PostgreSQL database per test, in place of `#[sqlx::test]` (ROADMAP row 29).
//!
//! Needs `PERSISTANT_TEST_DATABASE_URL` and a role with `CREATEDB`. Without the variable these tests
//! print a note and pass, **unless** `PERSISTANT_REQUIRE_DATABASE=1` (set by the CI job that provides a
//! server).

use persistant_relational::{Migration, RelationalBackend, Value};
use persistant_sqlx::{PgBackend, TestDb};

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
