// SPDX-License-Identifier: MIT OR Apache-2.0
//! `Value::Array` as a PostgreSQL array parameter (`= ANY($1)`): the types the elements show, a
//! NULL element, a described element type for an empty array and for `uuid[]` (through `Bound` and
//! `OnPool`, as `sqlx_query!` runs it), and every misuse a typed error. Skipped unless the server
//! variable is set (the same rule as the other server tests).

use persistant_relational::embed::{Binding, Query, Rendered, Target};
use persistant_relational::{
    ArrayElem, Bound, RelError, RelationalBackend, Row, Sql, ToValue, Value,
};
use persistant_sqlx::{OnPool, PgBackend};

fn url() -> Option<String> {
    let url = std::env::var("PERSISTANT_TEST_DATABASE_URL").ok();
    if url.is_none() {
        assert!(
            std::env::var("PERSISTANT_REQUIRE_DATABASE").as_deref() != Ok("1"),
            "PERSISTANT_REQUIRE_DATABASE=1 but PERSISTANT_TEST_DATABASE_URL is not set"
        );
        eprintln!("skipped: PERSISTANT_TEST_DATABASE_URL is not set");
    }
    url
}

const COUNT_ANY_M: Sql = Query {
    params: 1,
    constructs: &[],
    rendered: &[Rendered {
        target: Target::Postgres,
        sql: "SELECT count(*) FROM arr_m WHERE id = ANY($1)",
        param_order: &[1],
        param_bindings: &[Binding::NativeArray],
    }],
};

const COUNT_ANY: Sql = Query {
    params: 1,
    constructs: &[],
    rendered: &[Rendered {
        target: Target::Postgres,
        sql: "SELECT count(*) FROM arr_t WHERE id = ANY($1)",
        param_order: &[1],
        param_bindings: &[Binding::NativeArray],
    }],
};

const COUNT_UUID_ANY: Sql = Query {
    params: 1,
    constructs: &[],
    rendered: &[Rendered {
        target: Target::Postgres,
        sql: "SELECT count(*) FROM arr_u WHERE id = ANY($1)",
        param_order: &[1],
        param_bindings: &[Binding::NativeArray],
    }],
};

fn first_int(r: &Row) -> Result<i64, RelError> {
    match r.values[0] {
        Value::Int(n) => Ok(n),
        ref o => Err(RelError::Backend(format!("{o:?}"))),
    }
}

async fn count(
    db: &PgBackend,
    sql: &Sql,
    v: Value,
    hint: Option<ArrayElem>,
) -> Result<i64, RelError> {
    Bound::new(*sql, vec![v], first_int)
        .with_array_hints(vec![hint])
        .fetch_one(OnPool(db.pool()))
        .await
}

#[tokio::test]
async fn natural_arrays_nulls_and_the_empty_array() {
    let Some(url) = url() else { return };
    let db = PgBackend::connect(&url).await.unwrap();
    db.execute("DROP TABLE IF EXISTS arr_t", &[]).await.unwrap();
    db.execute(
        "CREATE TABLE arr_t (id bigint, t text, f double precision, b boolean)",
        &[],
    )
    .await
    .unwrap();
    db.execute(
        "INSERT INTO arr_t VALUES (1,'a',1.5,true),(2,'b',2.5,false),(3,NULL,NULL,NULL)",
        &[],
    )
    .await
    .unwrap();
    // int8[]: from a Vec<i64> through ToValue
    let ids: Vec<i64> = vec![1, 3, 99];
    assert_eq!(
        count(&db, &COUNT_ANY, ids.to_value(), None).await.unwrap(),
        2
    );
    // a NULL element is allowed and matches nothing
    let with_null = Value::Array(vec![Value::Int(2), Value::Null]);
    assert_eq!(count(&db, &COUNT_ANY, with_null, None).await.unwrap(), 1);
    // the empty array, with its described element type: matches nothing, and is not an error
    assert_eq!(
        count(&db, &COUNT_ANY, Value::Array(vec![]), Some(ArrayElem::Int8))
            .await
            .unwrap(),
        0
    );
    // text[], bool[], double precision[] through PgBackend::query directly
    for (sql, v, want) in [
        (
            "SELECT count(*) FROM arr_t WHERE t = ANY($1)",
            ["a", "b", "zz"][..].to_value(),
            2,
        ),
        (
            "SELECT count(*) FROM arr_t WHERE b = ANY($1)",
            [true][..].to_value(),
            1,
        ),
        (
            "SELECT count(*) FROM arr_t WHERE f = ANY($1)",
            [1.5_f64, 9.0][..].to_value(),
            1,
        ),
    ] {
        let rows = db.query(sql, &[v]).await.unwrap();
        assert_eq!(rows[0].values[0], Value::Int(want), "{sql}");
    }
}

#[tokio::test]
async fn a_uuid_array_needs_and_gets_its_described_type() {
    let Some(url) = url() else { return };
    let db = PgBackend::connect(&url).await.unwrap();
    db.execute("DROP TABLE IF EXISTS arr_u", &[]).await.unwrap();
    db.execute("CREATE TABLE arr_u (id uuid)", &[])
        .await
        .unwrap();
    let a = "00000000-0000-0000-0000-000000000001";
    let b = "00000000-0000-0000-0000-000000000002";
    let c = "00000000-0000-0000-0000-000000000003";
    for id in [a, b, c] {
        db.execute(&format!("INSERT INTO arr_u VALUES ('{id}')"), &[])
            .await
            .unwrap();
    }
    let two = Value::Array(vec![Value::Text(a.into()), Value::Text(c.into())]);
    // with the hint: uuid[] matches the uuid column
    assert_eq!(
        count(&db, &COUNT_UUID_ANY, two.clone(), Some(ArrayElem::Uuid))
            .await
            .unwrap(),
        2
    );
    // control: without it the same value is text[] and the server refuses `uuid = text`
    let e = count(&db, &COUNT_UUID_ANY, two, None).await.unwrap_err();
    assert!(format!("{e}").contains("uuid = text"), "{e}");
    // the empty uuid array binds with its type
    assert_eq!(
        count(
            &db,
            &COUNT_UUID_ANY,
            Value::Array(vec![]),
            Some(ArrayElem::Uuid)
        )
        .await
        .unwrap(),
        0
    );
}

#[tokio::test]
async fn misuse_is_a_typed_error_never_a_panic_or_a_cast() {
    let Some(url) = url() else { return };
    let db = PgBackend::connect(&url).await.unwrap();
    db.execute("DROP TABLE IF EXISTS arr_m", &[]).await.unwrap();
    db.execute("CREATE TABLE arr_m (id bigint)", &[])
        .await
        .unwrap();
    let cases: Vec<(&str, Value, Option<ArrayElem>)> = vec![
        (
            "mixed",
            Value::Array(vec![Value::Int(1), Value::Text("x".into())]),
            None,
        ),
        ("empty, no type", Value::Array(vec![]), None),
        ("all NULL, no type", Value::Array(vec![Value::Null]), None),
        (
            "nested",
            Value::Array(vec![Value::Array(vec![Value::Int(1)])]),
            None,
        ),
        (
            "text for int8",
            Value::Array(vec![Value::Text("1".into())]),
            Some(ArrayElem::Int8),
        ),
        (
            "not a uuid",
            Value::Array(vec![Value::Text("nope".into())]),
            Some(ArrayElem::Uuid),
        ),
    ];
    for (name, v, hint) in cases {
        let e = count(&db, &COUNT_ANY_M, v, hint).await.unwrap_err();
        assert!(matches!(e, RelError::Unsupported(_)), "{name}: {e:?}");
    }
    // an array where the statement has a scalar placeholder is refused by `prepare`, not sent
    const SCALAR: Sql = Query {
        params: 1,
        constructs: &[],
        rendered: &[Rendered {
            target: Target::Postgres,
            sql: "SELECT count(*) FROM arr_m WHERE id = $1",
            param_order: &[1],
            param_bindings: &[Binding::Scalar],
        }],
    };
    let e = count(&db, &SCALAR, Value::Array(vec![Value::Int(1)]), None)
        .await
        .unwrap_err();
    assert!(matches!(e, RelError::Unsupported(_)), "{e:?}");
}
