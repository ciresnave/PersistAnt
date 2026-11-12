// SPDX-License-Identifier: MIT OR Apache-2.0
//! `native::PgSession` against a real PostgreSQL: settings arrive as session state and are the typed
//! allow-list only, state outlives transactions, a dead connection is an error that stays an error
//! (no silent reconnect), no default timeout exists, and a connect failure does not carry the secret.

use persistant_relational::{ConnectionSecret, Isolation, RelError, RelationalTx, Value};
use persistant_sqlx::native::{IntervalStyle, PgSession, SessionSettings, TxOptions};
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

fn opts() -> ConnectOptions {
    ConnectOptions::default().with_tls(TlsPolicy::Disable)
}

async fn session(url: &str, settings: SessionSettings) -> PgSession {
    PgSession::connect(
        &ConnectionSecret::literal(url.to_string()),
        &opts(),
        settings,
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn settings_reach_the_server_and_the_plain_session_has_none() {
    let Some(url) = url() else { return };
    let mut s = session(
        &url,
        SessionSettings::new()
            .date_style_iso()
            .time_zone("America/New_York")
            .unwrap()
            .extra_float_digits(3)
            .unwrap()
            .interval_style(IntervalStyle::Iso8601),
    )
    .await;
    assert_eq!(s.show("datestyle").await.unwrap(), "ISO, MDY");
    assert_eq!(s.show("timezone").await.unwrap(), "America/New_York");
    assert_eq!(s.show("extra_float_digits").await.unwrap(), "3");
    assert_eq!(s.show("intervalstyle").await.unwrap(), "iso_8601");
    // control: a session with no settings has the server's, and no timeout of any kind
    let mut plain = session(&url, SessionSettings::new()).await;
    assert_eq!(plain.show("intervalstyle").await.unwrap(), "postgres");
    assert_ne!(plain.show("timezone").await.unwrap(), "America/New_York");
    assert_ne!(plain.show("extra_float_digits").await.unwrap(), "3");
    for t in [
        "statement_timeout",
        "lock_timeout",
        "idle_in_transaction_session_timeout",
    ] {
        assert_eq!(
            plain.show(t).await.unwrap(),
            "0",
            "{t} must be off by default"
        );
    }
}

#[tokio::test]
async fn a_timeout_is_set_only_when_asked() {
    let Some(url) = url() else { return };
    let mut s = session(
        &url,
        SessionSettings::new().statement_timeout(std::time::Duration::from_millis(1500)),
    )
    .await;
    assert_eq!(s.show("statement_timeout").await.unwrap(), "1500ms");
}

#[tokio::test]
async fn state_outlives_transactions_and_show_refuses_free_text() {
    let Some(url) = url() else { return };
    let mut s = session(
        &url,
        SessionSettings::new().time_zone("Asia/Tokyo").unwrap(),
    )
    .await;
    for _ in 0..3 {
        let mut tx = s
            .begin(TxOptions::new(Isolation::RepeatableRead).read_only())
            .await
            .unwrap();
        let rows = tx.query("SHOW timezone", &[]).await.unwrap();
        assert_eq!(rows[0].values[0], Value::Text("Asia/Tokyo".into()));
        tx.commit().await.unwrap();
    }
    assert_eq!(s.show("timezone").await.unwrap(), "Asia/Tokyo");
    for bad in ["", "TimeZone", "x; DROP TABLE t", "a b"] {
        assert!(s.show(bad).await.is_err(), "{bad:?}");
    }
}

#[tokio::test]
async fn a_dead_connection_is_an_error_and_stays_one() {
    let Some(url) = url() else { return };
    let mut s = session(
        &url,
        SessionSettings::new().time_zone("Asia/Tokyo").unwrap(),
    )
    .await;
    let pid = {
        let mut tx = s
            .begin(TxOptions::new(Isolation::ReadCommitted))
            .await
            .unwrap();
        let rows = tx
            .query("SELECT pg_backend_pid()::bigint", &[])
            .await
            .unwrap();
        tx.rollback().await.unwrap();
        match rows[0].values[0] {
            Value::Int(n) => n,
            ref o => panic!("{o:?}"),
        }
    };
    let admin = PgBackend::connect(&url).await.unwrap();
    use persistant_relational::RelationalBackend;
    admin
        .query("SELECT pg_terminate_backend($1::int)", &[Value::Int(pid)])
        .await
        .unwrap();
    // wait until the server has dropped it
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let first = s.show("timezone").await;
    assert!(
        first.is_err(),
        "the killed connection must fail, not reconnect: {first:?}"
    );
    // a reconnect would have a fresh session (TimeZone back to the server's) and succeed
    assert!(s.show("timezone").await.is_err(), "and it stays failed");
    assert!(
        s.begin(TxOptions::new(Isolation::ReadCommitted))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn a_connect_failure_does_not_carry_the_secret() {
    let Some(_) = url() else { return };
    let secret = ConnectionSecret::literal(
        "postgres://postgres:hunter2-secret@127.0.0.1:1/persistant".to_string(),
    );
    let e = PgSession::connect(&secret, &opts(), SessionSettings::new())
        .await
        .unwrap_err();
    let shown = format!("{e} {e:?}");
    assert!(!shown.contains("hunter2-secret"), "{shown}");
    assert!(
        matches!(e, RelError::Unavailable(_) | RelError::Backend(_)),
        "{e:?}"
    );
}
