// SPDX-License-Identifier: MIT OR Apache-2.0
//! One connection, one statement: a NULL for a uuid/timestamptz parameter, then a value for the same
//! parameter. The statement is prepared once per connection and cached. The translated text casts the
//! parameter to its type; formerly that was `CAST($n AS uuid)`, whose only type information made the
//! server infer `uuid` for a parameter first bound as an untyped NULL, so the later text-encoded value
//! was rejected with "incorrect binary data format in bind parameter". The cast is now
//! `CAST(CAST($n AS text) AS uuid)` and this test holds that. Each connection works in its own TEMP table,
//! so nothing is shared, left behind or dropped in the configured database. PostgreSQL only; skipped
//! unless its server variable is set.

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
    let u = Some(uuid::Uuid::from_u128(7));
    let t = chrono::DateTime::from_timestamp(1_700_000_000, 0);

    // NULL first (the statement is prepared with the parameters untyped), then values.
    let mut c = connect_with_table(&url).await;
    for (a, at) in [(None, None), (u, None), (None, t), (u, t), (None, None)] {
        let n = insert(&mut c, a, at).await.unwrap_or_else(|e| {
            panic!("binding ({a:?}, {at:?}) after a NULL on one connection failed: {e}")
        });
        assert_eq!(n, 1);
    }
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM persistant_nullbind")
        .fetch_one(&mut c)
        .await
        .unwrap();
    assert_eq!(rows, 5);

    // Control: the same statement, values only, on a fresh connection that never ran it with NULL.
    let mut c = connect_with_table(&url).await;
    for _ in 0..2 {
        insert(&mut c, u, t).await.unwrap();
    }
}

async fn connect_with_table(url: &str) -> sqlx::PgConnection {
    use sqlx::Connection;
    let mut c = sqlx::PgConnection::connect(url).await.unwrap();
    sqlx::query("CREATE TEMP TABLE persistant_nullbind (a uuid, at timestamptz)")
        .execute(&mut c)
        .await
        .unwrap();
    c
}
