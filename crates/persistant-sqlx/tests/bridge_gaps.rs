// SPDX-License-Identifier: MIT OR Apache-2.0
//! Humboldt's two statements through the bridge.
use persistant_macros::sqlx_query;
use persistant_sqlx::OnPool;

#[tokio::test]
async fn update_with_text_text_bool() {
    let Ok(url) = std::env::var("PERSISTANT_TEST_DATABASE_URL") else {
        return;
    };
    let pool = sqlx::PgPool::connect(&url).await.unwrap();
    sqlx::query("DROP TABLE IF EXISTS gap_oauth")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("CREATE TABLE gap_oauth (id bigint PRIMARY KEY, tenant text, email text, email_verified boolean NOT NULL DEFAULT false)")
        .execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO gap_oauth (id) VALUES (1)")
        .execute(&pool)
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
    .execute(OnPool(&pool))
    .await
    .unwrap();
    assert_eq!(n, 1);
    let row: (Option<String>, Option<String>, bool) =
        sqlx::query_as("SELECT tenant, email, email_verified FROM gap_oauth WHERE id = 1")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(row, (Some("t".into()), Some("a@b.c".into()), true));
    sqlx::query("DROP TABLE gap_oauth")
        .execute(&pool)
        .await
        .unwrap();
}
