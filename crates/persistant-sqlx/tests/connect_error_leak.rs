// SPDX-License-Identifier: MIT OR Apache-2.0
//! A failed connect must never put the PASSWORD of its connection string in the error it returns:
//! not in `Display`, not in `Debug`, not in any `source()` further down. Tried with a canary
//! password against a port nothing listens on, against text that is not a connection string, and,
//! where the CI servers exist, against a real server given the wrong password.
//!
//! Every path is tried through all the ways in: `connect(&str)`, `connect_secret`, `connect_with`.
//! Without a server the live cases print a note and pass, **unless** `PERSISTANT_REQUIRE_DATABASE=1`
//! / `PERSISTANT_REQUIRE_MYSQL=1` (set by the CI jobs that provide one).
//!
//! SQLite is not covered: it has no credential, and a path in its error is the caller's own.

use persistant_relational::{ConnectionSecret, RelError};
use persistant_sqlx::{ConnectOptions, PgBackend, TlsPolicy};
use std::error::Error;
use std::time::Duration;

/// Not a real password. Distinctive enough that its presence in text is not chance.
const CANARY: &str = "Cnry9x7Qz4Lw2Vb8";

/// Everything an error can show: `Display`, `Debug`, `{:#?}` and every `source()`.
fn everything(e: &RelError) -> String {
    let mut out = format!("{e}\n{e:?}\n{e:#?}");
    let mut src = e.source();
    while let Some(s) = src {
        out.push_str(&format!("\n{s}\n{s:?}"));
        src = s.source();
    }
    out
}

fn assert_clean(what: &str, e: &RelError) {
    let text = everything(e);
    eprintln!("{what}: {e}");
    assert!(
        !text.contains(CANARY),
        "{what}: the password leaked: {text}"
    );
}

fn fast() -> ConnectOptions {
    ConnectOptions {
        tls: TlsPolicy::Disable,
        max_connections: 1,
        acquire_timeout: Duration::from_secs(5),
    }
}

/// Nothing listens on port 1.
fn refused_pg() -> String {
    format!("postgres://app:{CANARY}@127.0.0.1:1/db?application_name=x")
}

#[tokio::test]
async fn postgres_to_a_closed_port_does_not_show_the_password() {
    let url = refused_pg();
    let e = PgBackend::connect(&url).await.expect_err("must fail");
    assert_clean("connect", &e);
    // `connect_secret` is `connect` after resolving; not repeated (each failed attempt waits out
    // sqlx's 30 s acquire timeout).
    let e = PgBackend::connect_with(&ConnectionSecret::literal(&url), &fast())
        .await
        .expect_err("must fail");
    assert_clean("connect_with", &e);
}

#[tokio::test]
async fn postgres_with_text_that_is_not_a_url_does_not_show_the_password() {
    for bad in [
        format!("postgres://app:{CANARY}@127.0.0.1:notaport/db"),
        format!("host=127.0.0.1 user=app password={CANARY}"),
        format!("postgres://app:{CANARY}@[::1/db"),
        CANARY.to_string(),
    ] {
        let e = PgBackend::connect(&bad).await.expect_err("must fail");
        assert_clean(&format!("connect({bad:.12}..)"), &e);
        let e = PgBackend::connect_with(&ConnectionSecret::literal(&bad), &fast())
            .await
            .expect_err("must fail");
        assert_clean(&format!("connect_with({bad:.12}..)"), &e);
    }
}

#[tokio::test]
async fn the_checker_can_fail() {
    // The control: an error that does hold the password is caught by the same reader.
    let e = RelError::Backend(format!("could not connect to postgres://app:{CANARY}@h/db"));
    assert!(everything(&e).contains(CANARY));
    let result = std::panic::catch_unwind(|| assert_clean("control", &e));
    assert!(result.is_err(), "assert_clean accepted a leaking error");
}

/// Replace the password in `scheme://user:password@host...` with the canary.
fn with_canary_password(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    let (userinfo, tail) = rest.rsplit_once('@')?;
    let user = userinfo.split_once(':').map_or(userinfo, |(u, _)| u);
    Some(format!("{scheme}://{user}:{CANARY}@{tail}"))
}

#[tokio::test]
async fn postgres_wrong_password_on_a_real_server_does_not_show_it() {
    let Ok(real) = std::env::var("PERSISTANT_TEST_DATABASE_URL") else {
        assert!(
            std::env::var("PERSISTANT_REQUIRE_DATABASE").as_deref() != Ok("1"),
            "PERSISTANT_REQUIRE_DATABASE=1 but PERSISTANT_TEST_DATABASE_URL is not set"
        );
        eprintln!("skipped: PERSISTANT_TEST_DATABASE_URL is not set");
        return;
    };
    let url = with_canary_password(&real).expect("the test URL has user@host");
    let e = PgBackend::connect(&url).await.expect_err("must fail");
    assert_clean("connect, wrong password", &e);
    let e = PgBackend::connect_with(&ConnectionSecret::literal(&url), &fast())
        .await
        .err();
    // The wrong password may be accepted by a trust-auth test server: only a failure is judged.
    if let Some(e) = e {
        assert_clean("connect_with, wrong password", &e);
    }
}

#[cfg(feature = "mysql")]
mod mysql {
    use super::*;
    use persistant_sqlx::MysqlBackend;

    #[tokio::test]
    async fn mysql_to_a_closed_port_does_not_show_the_password() {
        let url = format!("mysql://app:{CANARY}@127.0.0.1:1/db");
        let e = MysqlBackend::connect(&url).await.expect_err("must fail");
        assert_clean("connect", &e);
        let e = MysqlBackend::connect_secret(&ConnectionSecret::literal(&url))
            .await
            .expect_err("must fail");
        assert_clean("connect_secret", &e);
        let e = MysqlBackend::connect_with(&ConnectionSecret::literal(&url), &fast())
            .await
            .expect_err("must fail");
        assert_clean("connect_with", &e);
    }

    #[tokio::test]
    async fn mysql_with_text_that_is_not_a_url_does_not_show_the_password() {
        for bad in [
            format!("mysql://app:{CANARY}@127.0.0.1:notaport/db"),
            CANARY.to_string(),
        ] {
            let e = MysqlBackend::connect(&bad).await.expect_err("must fail");
            assert_clean("connect", &e);
            let e = MysqlBackend::connect_with(&ConnectionSecret::literal(&bad), &fast())
                .await
                .expect_err("must fail");
            assert_clean("connect_with", &e);
        }
    }

    #[tokio::test]
    async fn mysql_wrong_password_on_a_real_server_does_not_show_it() {
        let Ok(real) = std::env::var("PERSISTANT_TEST_MYSQL_URL") else {
            assert!(
                std::env::var("PERSISTANT_REQUIRE_MYSQL").as_deref() != Ok("1"),
                "PERSISTANT_REQUIRE_MYSQL=1 but PERSISTANT_TEST_MYSQL_URL is not set"
            );
            eprintln!("skipped: PERSISTANT_TEST_MYSQL_URL is not set");
            return;
        };
        let url = with_canary_password(&real).expect("the test URL has user@host");
        let e = MysqlBackend::connect(&url).await.err();
        if let Some(e) = e {
            assert_clean("connect, wrong password", &e);
        }
    }
}
