// SPDX-License-Identifier: MIT OR Apache-2.0
//! `FromValue` / `ToValue` for `Uuid`, `chrono` and `serde_json::Value` against a real PostgreSQL:
//! each is read from the text the driver returns and sent back as text with a cast on the placeholder.
//!
//! Needs `PERSISTANT_TEST_DATABASE_URL`; without it the test prints a note and passes, unless
//! `PERSISTANT_REQUIRE_DATABASE=1` (the same rule as `kv_on_postgres.rs`).

use chrono::{DateTime, NaiveDate, NaiveDateTime, Utc};
use persistant_relational::{FromRow, FromValue, RelationalBackend, ToValue, Value};
use persistant_sqlx::PgBackend;

async fn backend() -> Option<PgBackend> {
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

/// The shape of a Humboldt DTO: a uuid key, a timestamp, an optional date, a json document.
#[derive(Debug, PartialEq, persistant_macros::FromRow)]
#[persistant(crate = ::persistant_relational)]
struct Event {
    id: uuid::Uuid,
    at: DateTime<Utc>,
    day: Option<NaiveDate>,
    doc: serde_json::Value,
}

#[tokio::test]
async fn a_struct_with_uuid_timestamp_date_and_json_fields_reads_a_row() {
    let Some(db) = backend().await else { return };
    let rows = db
        .query(
            "SELECT 'a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11'::uuid AS id, \
             '2026-10-17 12:34:56.789+00'::timestamptz AS at, NULL::date AS day, \
             '{\"a\": [1, 2]}'::jsonb AS doc",
            &[],
        )
        .await
        .unwrap();
    let e = Event::from_row(&rows[0]).unwrap();
    assert_eq!(e.id.to_string(), "a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11");
    assert_eq!(
        e.at.to_value(),
        Value::Text("2026-10-17T12:34:56.789Z".into())
    );
    assert_eq!(e.day, None);
    assert_eq!(e.doc["a"][1], 2);
}

#[tokio::test]
async fn parameters_sent_as_text_with_a_cast_come_back_equal() {
    let Some(db) = backend().await else { return };
    let id = uuid::Uuid::parse_str("a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11").unwrap();
    let at = DateTime::<Utc>::from_value(&Value::Text("2026-10-17T12:34:56.789Z".into())).unwrap();
    let day = NaiveDate::from_value(&Value::Text("2026-10-17".into())).unwrap();
    let naive = NaiveDateTime::from_value(&Value::Text("2026-10-17T12:34:56.500".into())).unwrap();
    let doc = serde_json::json!({"k": "v"});
    let rows = db
        .query(
            "SELECT CAST($1 AS uuid), CAST($2 AS timestamptz), CAST($3 AS date), \
             CAST($4 AS timestamp), CAST($5 AS jsonb), CAST($6 AS uuid)",
            &[
                id.to_value(),
                at.to_value(),
                day.to_value(),
                naive.to_value(),
                doc.to_value(),
                Option::<uuid::Uuid>::None.to_value(),
            ],
        )
        .await
        .unwrap();
    let r = &rows[0];
    assert_eq!(uuid::Uuid::from_value(&r.values[0]).unwrap(), id);
    assert_eq!(DateTime::<Utc>::from_value(&r.values[1]).unwrap(), at);
    assert_eq!(NaiveDate::from_value(&r.values[2]).unwrap(), day);
    assert_eq!(NaiveDateTime::from_value(&r.values[3]).unwrap(), naive);
    assert_eq!(serde_json::Value::from_value(&r.values[4]).unwrap(), doc);
    assert_eq!(
        Option::<uuid::Uuid>::from_value(&r.values[5]).unwrap(),
        None
    );
}
