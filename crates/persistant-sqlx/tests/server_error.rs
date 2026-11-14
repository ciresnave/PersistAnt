// SPDX-License-Identifier: MIT OR Apache-2.0
//! What the server said survives in `RelError`: SQLSTATE and message for a unique and a check
//! violation, for a trigger's `RAISE` with a custom code and with `check_violation`, through the pool
//! backend and through `OnConn`; a failure that never reached the server has none.
use persistant_relational::{RelError, RelationalBackend};
use persistant_sqlx::{OnConn, PgBackend};
use sqlx::Connection;

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

const SETUP: &[&str] = &[
    "CREATE TEMP TABLE se_t (id int PRIMARY KEY, n int CHECK (n > 0), tag text)",
    "CREATE FUNCTION pg_temp.se_guard() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN \
       IF NEW.tag = 'custom' THEN RAISE EXCEPTION 'cap of 4 reached' USING ERRCODE = 'PA001'; END IF; \
       IF NEW.tag = 'chk' THEN RAISE EXCEPTION 'tag refused' USING ERRCODE = 'check_violation'; END IF; \
       RETURN NEW; END $$",
    "CREATE TRIGGER se_guard BEFORE INSERT ON se_t FOR EACH ROW EXECUTE FUNCTION pg_temp.se_guard()",
];

async fn ex(c: &mut sqlx::PgConnection, sql: &str) -> Result<u64, RelError> {
    OnConn(c).execute(sql, &[]).await
}

fn sv(e: &RelError) -> (&str, &str) {
    let s = e
        .server_error()
        .unwrap_or_else(|| panic!("no server error on {e:?}"));
    (s.sqlstate.as_str(), s.message.as_str())
}

#[tokio::test]
async fn the_servers_code_and_message_stay_on_the_error_through_onconn() {
    let Some(url) = url() else { return };
    let mut c = sqlx::PgConnection::connect(&url).await.unwrap();
    for s in SETUP {
        sqlx::query(sqlx::AssertSqlSafe(*s))
            .execute(&mut c)
            .await
            .unwrap();
    }
    ex(&mut c, "INSERT INTO se_t VALUES (1, 1, 'ok')")
        .await
        .unwrap();

    // a unique violation: its code and message, and the constraint named once
    let dup = ex(&mut c, "INSERT INTO se_t VALUES (1, 1, 'ok')")
        .await
        .unwrap_err();
    assert!(matches!(dup, RelError::UniqueViolation { .. }), "{dup:?}");
    assert_eq!(sv(&dup).0, "23505");
    assert!(sv(&dup).1.contains("se_t_pkey"), "{dup:?}");
    assert_eq!(dup.constraint(), Some("se_t_pkey"));
    assert!(
        matches!(&dup, RelError::UniqueViolation { constraint, .. } if constraint.as_deref() == dup.constraint())
    );

    // a CHECK violation
    let chk = ex(&mut c, "INSERT INTO se_t VALUES (2, 0, 'ok')")
        .await
        .unwrap_err();
    assert!(matches!(chk, RelError::CheckViolation { .. }), "{chk:?}");
    assert_eq!(sv(&chk).0, "23514");

    // a trigger's RAISE with a custom SQLSTATE keeps the code and the message
    let custom = ex(&mut c, "INSERT INTO se_t VALUES (3, 1, 'custom')")
        .await
        .unwrap_err();
    assert!(matches!(custom, RelError::Database { .. }), "{custom:?}");
    assert_eq!(sv(&custom), ("PA001", "cap of 4 reached"));

    // a trigger's RAISE with check_violation is the check category, message kept
    let tchk = ex(&mut c, "INSERT INTO se_t VALUES (4, 1, 'chk')")
        .await
        .unwrap_err();
    assert!(matches!(tchk, RelError::CheckViolation { .. }), "{tchk:?}");
    assert_eq!(sv(&tchk), ("23514", "tag refused"));

    // control: a failure that did not come from the server has no server error
    let none = RelError::Backend("decode".into());
    assert!(none.server_error().is_none());
}

#[tokio::test]
async fn the_servers_code_and_message_stay_on_the_error_through_the_pool_backend() {
    let Some(url) = url() else { return };
    let db = PgBackend::connect(&url).await.unwrap();
    db.execute("DROP TABLE IF EXISTS persistant_server_error_t", &[])
        .await
        .unwrap();
    db.execute(
        "CREATE TABLE persistant_server_error_t (id int PRIMARY KEY, tag text)",
        &[],
    )
    .await
    .unwrap();
    db.execute(
        "CREATE OR REPLACE FUNCTION persistant_server_error_f() RETURNS trigger LANGUAGE plpgsql AS \
         $$ BEGIN RAISE EXCEPTION 'cap of 4 reached' USING ERRCODE = 'PA001'; END $$",
        &[],
    )
    .await
    .unwrap();
    db.execute(
        "CREATE TRIGGER persistant_server_error_g BEFORE INSERT ON persistant_server_error_t \
         FOR EACH ROW EXECUTE FUNCTION persistant_server_error_f()",
        &[],
    )
    .await
    .unwrap();
    let e = db
        .execute("INSERT INTO persistant_server_error_t VALUES (1, 'x')", &[])
        .await
        .unwrap_err();
    let got = (sv(&e).0.to_string(), sv(&e).1.to_string());
    let _ = db
        .execute("DROP TABLE persistant_server_error_t", &[])
        .await;
    let _ = db
        .execute("DROP FUNCTION persistant_server_error_f()", &[])
        .await;
    assert!(matches!(e, RelError::Database { .. }), "{e:?}");
    assert_eq!(got, ("PA001".to_string(), "cap of 4 reached".to_string()));
}
