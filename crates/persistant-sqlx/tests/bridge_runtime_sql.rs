// SPDX-License-Identifier: MIT OR Apache-2.0
//! Runtime SQL on a held sqlx connection and pool (`OnConn::query` and friends) and the typed
//! `Row::get`: a statement text written at run time, `Value` arguments, rows read by index. PostgreSQL
//! only; skipped unless its server variable is set.

use persistant_relational::{RelError, Value};
use persistant_sqlx::{OnConn, OnPool};
use sqlx::Connection;

const UUID_A: &str = "00000000-0000-0000-0000-00000000000a";

fn url() -> Option<String> {
    let Ok(url) = std::env::var("PERSISTANT_TEST_DATABASE_URL") else {
        assert!(
            std::env::var("PERSISTANT_REQUIRE_DATABASE").as_deref() != Ok("1"),
            "PERSISTANT_REQUIRE_DATABASE=1 but PERSISTANT_TEST_DATABASE_URL is not set"
        );
        eprintln!("skipped: PERSISTANT_TEST_DATABASE_URL is not set");
        return None;
    };
    Some(url)
}

async fn conn_with_table(url: &str) -> sqlx::PgConnection {
    let mut c = sqlx::PgConnection::connect(url).await.unwrap();
    sqlx::query(
        "CREATE TEMP TABLE rt_sql (id bigint PRIMARY KEY, note text, who uuid, ok boolean)",
    )
    .execute(&mut c)
    .await
    .unwrap();
    c
}

#[tokio::test]
async fn execute_query_and_typed_reads_on_a_connection() {
    let Some(url) = url() else { return };
    let mut c = conn_with_table(&url).await;
    let mut on = OnConn(&mut c);

    let n = on
        .execute(
            "INSERT INTO rt_sql (id, note, ok) VALUES ($1, $2, $3), ($4, $5, $6)",
            &[
                Value::Int(1),
                Value::Text("a".into()),
                Value::Bool(true),
                Value::Int(2),
                Value::Null,
                Value::Bool(false),
            ],
        )
        .await
        .unwrap();
    assert_eq!(n, 2);

    let rows = on
        .query("SELECT id, note, ok FROM rt_sql ORDER BY id", &[])
        .await
        .unwrap();
    assert_eq!(rows.len(), 2);
    let (id, note, ok): (i64, Option<String>, bool) = (
        rows[0].get(0).unwrap(),
        rows[0].get(1).unwrap(),
        rows[0].get(2).unwrap(),
    );
    assert_eq!((id, note.as_deref(), ok), (1, Some("a"), true));
    // NULL: `get_opt` and `get::<Option<_>>` say None; a plain `get::<String>` is an error naming the column.
    assert_eq!(rows[1].get_opt::<String>(1).unwrap(), None);
    assert_eq!(rows[1].get::<Option<String>>(1).unwrap(), None);
    match rows[1].get::<String>(1) {
        Err(RelError::Backend(m)) => assert!(m.contains("column 2"), "{m}"),
        other => panic!("expected an error naming column 2, got {other:?}"),
    }
    // A column that is not there.
    match rows[0].get::<i64>(9) {
        Err(RelError::Backend(m)) => assert!(m.contains("3 columns"), "{m}"),
        other => panic!("{other:?}"),
    }

    let one = on
        .query_one("SELECT id FROM rt_sql WHERE id = $1", &[Value::Int(2)])
        .await
        .unwrap();
    assert_eq!(one.get::<i64>(0).unwrap(), 2);
    assert!(
        on.query_optional("SELECT id FROM rt_sql WHERE id = $1", &[Value::Int(99)])
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        matches!(
            on.query_one("SELECT id FROM rt_sql WHERE id = $1", &[Value::Int(99)]).await,
            Err(RelError::Backend(m)) if m.contains("no rows")
        ),
        "query_one on no rows is an error"
    );
}

#[tokio::test]
async fn a_uuid_cast_through_text_survives_null_then_a_value() {
    let Some(url) = url() else { return };
    let mut c = conn_with_table(&url).await;
    let mut on = OnConn(&mut c);
    let ins = "INSERT INTO rt_sql (id, who) VALUES ($1, CAST(CAST($2 AS text) AS uuid))";
    // NULL first, then a value, on the one connection (the same statement text).
    on.execute(ins, &[Value::Int(1), Value::Null])
        .await
        .unwrap();
    on.execute(ins, &[Value::Int(2), Value::Text(UUID_A.into())])
        .await
        .unwrap();
    let r = on
        .query_one(
            "SELECT who::text FROM rt_sql WHERE id = $1",
            &[Value::Int(2)],
        )
        .await
        .unwrap();
    assert_eq!(r.get::<String>(0).unwrap(), UUID_A);

    // Why the doc says "through text": a bare `$2::uuid` is inferred as uuid when NULL is bound first, and
    // the cached statement then rejects the text value.
    let bare = "INSERT INTO rt_sql (id, who) VALUES ($1, $2::uuid)";
    on.execute(bare, &[Value::Int(3), Value::Null])
        .await
        .unwrap();
    assert!(
        on.execute(bare, &[Value::Int(4), Value::Text(UUID_A.into())])
            .await
            .is_err(),
        "the bare cast fails after a NULL (the reason for the documented form)"
    );
}

#[tokio::test]
async fn the_same_calls_work_on_a_pool() {
    let Some(url) = url() else { return };
    let pool = sqlx::PgPool::connect(&url).await.unwrap();
    let mut on = OnPool(&pool);
    let r = on
        .query_one(
            "SELECT $1::bigint + 1, $2::text",
            &[Value::Int(41), Value::Text("x".into())],
        )
        .await
        .unwrap();
    assert_eq!(
        (r.get::<i64>(0).unwrap(), r.get::<String>(1).unwrap()),
        (42, "x".to_string())
    );
    assert_eq!(on.execute("SELECT 1", &[]).await.unwrap(), 1);
}
