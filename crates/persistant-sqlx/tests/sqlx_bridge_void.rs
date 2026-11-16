// SPDX-License-Identifier: MIT OR Apache-2.0
//! A `void` result through `sqlx_query!` (issue #229, Humboldt gap 3): `SELECT void_fn($1)`. sqlx
//! describes the column as `Void` (Rust `()`); PostgreSQL sends a zero-length, non-NULL datum for it.
//! The bridge has to read it as `()` (it had no `FromValue`, so the statement did not compile), and
//! the driver has to carry the datum (it refused the type). PostgreSQL only; skipped unless its server
//! variable is set (the same rule as the other server tests).

use persistant_macros::sqlx_query;
use persistant_sqlx::OnPool;

#[tokio::test]
async fn a_void_function_result_is_unit() {
    let Ok(url) = std::env::var("PERSISTANT_TEST_DATABASE_URL") else {
        assert!(
            std::env::var("PERSISTANT_REQUIRE_DATABASE").as_deref() != Ok("1"),
            "PERSISTANT_REQUIRE_DATABASE=1 but PERSISTANT_TEST_DATABASE_URL is not set"
        );
        eprintln!("skipped: PERSISTANT_TEST_DATABASE_URL is not set");
        return;
    };
    let pool = sqlx::PgPool::connect(&url).await.unwrap();
    sqlx::query("DROP TABLE IF EXISTS persistant_void_calls")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("CREATE TABLE persistant_void_calls (n bigint)")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "CREATE OR REPLACE FUNCTION persistant_void_fn(i bigint) RETURNS void \
         LANGUAGE sql AS 'INSERT INTO persistant_void_calls VALUES (i)'",
    )
    .execute(&pool)
    .await
    .unwrap();

    let row = sqlx_query!(
        crate = ::persistant_relational,
        sqlx = none,
        "SELECT persistant_void_fn($1)",
        7_i64
    )
    .fetch_one(OnPool(&pool))
    .await
    .unwrap();
    // The statement ran (its side effect is there) and the unit column read as `()`.
    // sqlx cannot say a void column is non-null, so `sqlx_query!` types it `Option<()>`: NULL is `None`.
    assert_eq!(row.persistant_void_fn, None);
    let calls: i64 = sqlx::query_scalar("SELECT count(*) FROM persistant_void_calls WHERE n = 7")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(calls, 1);
}

/// The driver on its own: a `void` column is carried (as NULL, the only value a unit has).
#[tokio::test]
async fn the_driver_carries_a_void_column_as_null() {
    use persistant_relational::RelationalBackend;
    let Ok(url) = std::env::var("PERSISTANT_TEST_DATABASE_URL") else {
        assert!(
            std::env::var("PERSISTANT_REQUIRE_DATABASE").as_deref() != Ok("1"),
            "PERSISTANT_REQUIRE_DATABASE=1 but PERSISTANT_TEST_DATABASE_URL is not set"
        );
        eprintln!("skipped: PERSISTANT_TEST_DATABASE_URL is not set");
        return;
    };
    let db = persistant_sqlx::PgBackend::connect(&url).await.unwrap();
    let rows = db
        .query("SELECT pg_sleep(0) AS slept, 1::bigint AS one", &[])
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get::<()>(0).unwrap(), ());
    assert_eq!(rows[0].get::<Option<()>>(0).unwrap(), None);
    assert_eq!(rows[0].get::<i64>(1).unwrap(), 1);
    // A non-NULL value is not a void.
    assert!(rows[0].get::<()>(1).is_err());
}
