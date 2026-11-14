// SPDX-License-Identifier: MIT OR Apache-2.0
//! `Vec<Uuid>` parameters (`= ANY($1)`) through `sqlx_query!` on a held sqlx connection: the element
//! type comes from sqlx's described parameter type (`UuidArray`), so a uuid array binds as
//! `uuid[]` and an EMPTY array binds with its described type rather than failing for want of elements.
//! PostgreSQL only; skipped unless its server variable is set.

use persistant_macros::sqlx_query;
use persistant_sqlx::OnConn;
use sqlx::Connection;

#[tokio::test]
async fn uuid_arrays_bind_with_the_described_type() {
    let Ok(url) = std::env::var("PERSISTANT_TEST_DATABASE_URL") else {
        assert!(
            std::env::var("PERSISTANT_REQUIRE_DATABASE").as_deref() != Ok("1"),
            "PERSISTANT_REQUIRE_DATABASE=1 but PERSISTANT_TEST_DATABASE_URL is not set"
        );
        eprintln!("skipped: PERSISTANT_TEST_DATABASE_URL is not set");
        return;
    };
    let mut c = sqlx::PgConnection::connect(&url).await.unwrap();
    sqlx::query("CREATE TEMP TABLE persistant_arr (id uuid PRIMARY KEY)")
        .execute(&mut c)
        .await
        .unwrap();

    let a = uuid::Uuid::from_u128(1);
    let b = uuid::Uuid::from_u128(2);
    for id in [a, b] {
        sqlx::query("INSERT INTO persistant_arr (id) VALUES (CAST($1 AS uuid))")
            .bind(id.to_string())
            .execute(&mut c)
            .await
            .unwrap();
    }

    let count = |ids: Vec<uuid::Uuid>| {
        sqlx_query!(
            crate = ::persistant_relational,
            sqlx = none,
            "SELECT count(*) AS \"n!\" FROM persistant_arr WHERE id = ANY($1)",
            ids
        )
    };
    let n = count(vec![a, b, uuid::Uuid::from_u128(3)])
        .fetch_one(OnConn(&mut c))
        .await
        .unwrap();
    assert_eq!(n.n, 2);
    let n = count(vec![a]).fetch_one(OnConn(&mut c)).await.unwrap();
    assert_eq!(n.n, 1);
    // Empty: no elements to show the type; the described `uuid[]` is what binds.
    let n = count(Vec::new()).fetch_one(OnConn(&mut c)).await.unwrap();
    assert_eq!(n.n, 0);
}
