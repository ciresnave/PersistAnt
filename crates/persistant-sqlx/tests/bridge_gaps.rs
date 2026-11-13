// SPDX-License-Identifier: MIT OR Apache-2.0
//! Humboldt's two statements through the bridge.
use persistant_macros::sqlx_query;
use persistant_sqlx::OnConn;
use sqlx::Connection;

#[tokio::test]
async fn update_with_text_text_bool() {
    let Ok(url) = std::env::var("PERSISTANT_TEST_DATABASE_URL") else {
        assert!(
            std::env::var("PERSISTANT_REQUIRE_DATABASE").as_deref() != Ok("1"),
            "PERSISTANT_REQUIRE_DATABASE=1 but PERSISTANT_TEST_DATABASE_URL is not set"
        );
        eprintln!("skipped: PERSISTANT_TEST_DATABASE_URL is not set");
        return;
    };
    // A TEMP table on one connection: nothing is shared with a parallel run, nothing pre-existing is
    // dropped, and nothing is left behind.
    let mut c = sqlx::PgConnection::connect(&url).await.unwrap();
    sqlx::query("CREATE TEMP TABLE gap_oauth (id bigint PRIMARY KEY, tenant text, email text, email_verified boolean NOT NULL DEFAULT false)")
        .execute(&mut c).await.unwrap();
    sqlx::query("INSERT INTO gap_oauth (id) VALUES (1)")
        .execute(&mut c)
        .await
        .unwrap();
    let email: Option<&str> = Some("a@b.c");
    let n = sqlx_query!(
        crate = ::persistant_relational,
        sqlx = none,
        "UPDATE gap_oauth SET tenant=$2, email=$3, email_verified=$4 WHERE id=$1",
        1_i64,
        "t",
        email,
        true
    )
    .execute(OnConn(&mut c))
    .await
    .unwrap();
    assert_eq!(n, 1);
    let row: (Option<String>, Option<String>, bool) =
        sqlx::query_as("SELECT tenant, email, email_verified FROM gap_oauth WHERE id = 1")
            .fetch_one(&mut c)
            .await
            .unwrap();
    assert_eq!(row, (Some("t".into()), Some("a@b.c".into()), true));
}
