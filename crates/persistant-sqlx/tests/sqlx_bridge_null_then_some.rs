// SPDX-License-Identifier: MIT OR Apache-2.0
//! One connection, one statement: a NULL for a uuid/timestamptz parameter, then a value for the same
//! parameter. The statement is prepared once per connection and cached; the translated text casts the
//! parameter (`CAST($n AS uuid)`), and a parameter whose only type information is that cast is inferred
//! as `uuid` when first bound as an untyped NULL, so the later text-encoded value was rejected with
//! "incorrect binary data format in bind parameter". PostgreSQL only; skipped unless its server
//! variable is set.

use persistant_macros::sqlx_query;
use persistant_sqlx::OnConn;

async fn insert(
    c: &mut sqlx::PgConnection,
    a: Option<uuid::Uuid>,
    at: Option<chrono::DateTime<chrono::Utc>>,
) -> Result<u64, persistant_relational::RelError> {
    sqlx_query!(
        crate = ::persistant_relational,
        sqlx = none,
        "INSERT INTO persistant_nullbind (a, at) VALUES ($1, $2)",
        a,
        at
    )
    .execute(OnConn(c))
    .await
}

#[tokio::test]
async fn null_then_some_on_one_connection() {
    let Ok(url) = std::env::var("PERSISTANT_TEST_DATABASE_URL") else {
        assert!(
            std::env::var("PERSISTANT_REQUIRE_DATABASE").as_deref() != Ok("1"),
            "PERSISTANT_REQUIRE_DATABASE=1 but PERSISTANT_TEST_DATABASE_URL is not set"
        );
        eprintln!("skipped: PERSISTANT_TEST_DATABASE_URL is not set");
        return;
    };
    let pool = sqlx::PgPool::connect(&url).await.unwrap();
    sqlx::query("DROP TABLE IF EXISTS persistant_nullbind")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("CREATE TABLE persistant_nullbind (a uuid, at timestamptz)")
        .execute(&pool)
        .await
        .unwrap();

    let u = Some(uuid::Uuid::from_u128(7));
    let t = chrono::DateTime::from_timestamp(1_700_000_000, 0);
    let mut c = pool.acquire().await.unwrap();
    // NULL first (the statement is prepared with the parameters untyped), then values.
    for (a, at) in [(None, None), (u, None), (None, t), (u, t), (None, None)] {
        let n = insert(&mut c, a, at).await.unwrap_or_else(|e| {
            panic!("binding ({a:?}, {at:?}) after a NULL on one connection failed: {e}")
        });
        assert_eq!(n, 1);
    }
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM persistant_nullbind")
        .fetch_one(&mut *c)
        .await
        .unwrap();
    assert_eq!(rows, 5);
    drop(c);

    // Control: the same statement, always a value, on the same connection, never failed.
    let mut c = pool.acquire().await.unwrap();
    for _ in 0..2 {
        insert(&mut c, u, t).await.unwrap();
    }
}
