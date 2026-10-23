// SPDX-License-Identifier: MIT OR Apache-2.0
//! `sqlx_query!` statements run on the sqlx pool and sqlx transaction a program already holds
//! (`OnPool`, `OnConn`; `docs/DESIGN-SQLX-BRIDGE.md` section 6): Uuid and timestamptz parameters are cast
//! from sqlx's recorded types, rows are decoded as `PgBackend` decodes them, and a statement inside an
//! sqlx transaction sees, and is undone with, the rest of that transaction. PostgreSQL only; skipped
//! unless its server variable is set (the same rule as the other server tests).

use persistant_macros::sqlx_query;
use persistant_sqlx::{OnConn, OnPool};

#[tokio::test]
async fn on_a_pool_and_inside_an_sqlx_transaction() {
    let Ok(url) = std::env::var("PERSISTANT_TEST_DATABASE_URL") else {
        assert!(
            std::env::var("PERSISTANT_REQUIRE_DATABASE").as_deref() != Ok("1"),
            "PERSISTANT_REQUIRE_DATABASE=1 but PERSISTANT_TEST_DATABASE_URL is not set"
        );
        eprintln!("skipped: PERSISTANT_TEST_DATABASE_URL is not set");
        return;
    };
    let pool = sqlx::PgPool::connect(&url).await.unwrap();
    sqlx::query("DROP TABLE IF EXISTS persistant_sqlxb")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "CREATE TABLE persistant_sqlxb (id uuid PRIMARY KEY, at timestamptz NOT NULL, label text)",
    )
    .execute(&pool)
    .await
    .unwrap();

    let id = uuid::Uuid::from_u128(0x1111_1111_2222_3333_4444_5555_6666_7777);
    let at = chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap();
    let n = sqlx_query!(
        crate = ::persistant_relational,
        sqlx = none,
        "INSERT INTO persistant_sqlxb (id, at, label) VALUES ($1, $2, $3)",
        id,
        at,
        Some("first")
    )
    .execute(OnPool(&pool))
    .await
    .unwrap();
    assert_eq!(n, 1);

    let row = sqlx_query!(
        crate = ::persistant_relational,
        sqlx = none,
        "SELECT id, at, label FROM persistant_sqlxb WHERE id = $1",
        id
    )
    .fetch_one(OnPool(&pool))
    .await
    .unwrap();
    assert_eq!(
        (row.id, row.at, row.label.as_deref()),
        (id, at, Some("first"))
    );

    // Inside a transaction the program holds: the bridge statement and a plain sqlx one share it.
    let id2 = uuid::Uuid::from_u128(0x8888_8888_9999_aaaa_bbbb_cccc_dddd_eeee);
    let mut tx = pool.begin().await.unwrap();
    sqlx_query!(
        crate = ::persistant_relational,
        sqlx = none,
        "INSERT INTO persistant_sqlxb (id, at, label) VALUES ($1, $2, $3)",
        id2,
        at,
        None::<String>
    )
    .execute(OnConn(&mut tx))
    .await
    .unwrap();
    let seen: i64 = sqlx::query_scalar("SELECT count(*) FROM persistant_sqlxb")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    assert_eq!(seen, 2, "the bridge insert is part of the sqlx transaction");
    let inside = sqlx_query!(
        crate = ::persistant_relational,
        sqlx = none,
        "SELECT id, at, label FROM persistant_sqlxb WHERE id = $1",
        id2
    )
    .fetch_optional(OnConn(&mut tx))
    .await
    .unwrap();
    assert_eq!(inside.map(|r| r.label), Some(None));
    tx.rollback().await.unwrap();

    // Rolled back with the transaction.
    let after = sqlx_query!(
        crate = ::persistant_relational,
        sqlx = none,
        "SELECT id, at, label FROM persistant_sqlxb WHERE id = $1",
        id2
    )
    .fetch_optional(OnPool(&pool))
    .await
    .unwrap();
    assert!(after.is_none());

    sqlx::query("DROP TABLE persistant_sqlxb")
        .execute(&pool)
        .await
        .unwrap();
}
