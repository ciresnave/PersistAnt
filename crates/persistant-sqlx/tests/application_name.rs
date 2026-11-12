// SPDX-License-Identifier: MIT OR Apache-2.0
//! `ConnectOptions::application_name` reaches the server (`SHOW application_name`), overrides the
//! URL's own `application_name`, and leaves it alone when unset. PostgreSQL only; skipped unless its
//! server variable is set (the same rule as the other server tests).

use persistant_relational::{ConnectionSecret, RelationalBackend, Value};
use persistant_sqlx::{ConnectOptions, PgBackend, TlsPolicy};

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

async fn name_on_server(url: &str, options: &ConnectOptions) -> String {
    let db = PgBackend::connect_with(&ConnectionSecret::literal(url.to_string()), options)
        .await
        .unwrap();
    let rows = db.query("SHOW application_name", &[]).await.unwrap();
    match rows[0].values[0].clone() {
        Value::Text(t) => t,
        other => panic!("application_name is {other:?}"),
    }
}

fn plain() -> ConnectOptions {
    ConnectOptions::default().with_tls(TlsPolicy::Disable)
}

#[tokio::test]
async fn the_option_reaches_the_server() {
    let Some(url) = url() else { return };
    let o = plain().with_application_name("db-wipe");
    assert_eq!(name_on_server(&url, &o).await, "db-wipe");
}

#[tokio::test]
async fn the_option_wins_over_the_urls_own() {
    let Some(url) = url() else { return };
    let sep = if url.contains('?') { '&' } else { '?' };
    let url = format!("{url}{sep}application_name=from-the-url");
    let o = plain().with_application_name("from-the-option");
    assert_eq!(name_on_server(&url, &o).await, "from-the-option");
}

#[tokio::test]
async fn unset_leaves_the_urls_own_alone() {
    let Some(url) = url() else { return };
    let sep = if url.contains('?') { '&' } else { '?' };
    let url = format!("{url}{sep}application_name=from-the-url");
    assert_eq!(name_on_server(&url, &plain()).await, "from-the-url");
}
