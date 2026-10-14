// SPDX-License-Identifier: MIT OR Apache-2.0
//! TLS policy against a real PostgreSQL that has `ssl = on` with a self-signed certificate
//! (`localhost`), and against one that has not.
//!
//! Needs `PERSISTANT_TEST_TLS_DATABASE_URL` (the TLS server, host `localhost`),
//! `PERSISTANT_TEST_TLS_CA` (its certificate, PEM) and `PERSISTANT_TEST_DATABASE_URL` (the plain
//! server). Skipped without them unless `PERSISTANT_REQUIRE_TLS_DATABASE=1` (set by the CI job
//! that starts both servers), in which case a missing variable is a failure.

use persistant_relational::{ConnectionSecret, RelError};
use persistant_sqlx::{ConnectOptions, PgBackend, TlsPolicy};
use std::path::PathBuf;

struct Servers {
    tls_url: String,
    ca: PathBuf,
    plain_url: String,
}

fn servers() -> Option<Servers> {
    let get = |k: &str| std::env::var(k).ok();
    match (
        get("PERSISTANT_TEST_TLS_DATABASE_URL"),
        get("PERSISTANT_TEST_TLS_CA"),
        get("PERSISTANT_TEST_DATABASE_URL"),
    ) {
        (Some(tls_url), Some(ca), Some(plain_url)) => Some(Servers {
            tls_url,
            ca: PathBuf::from(ca),
            plain_url,
        }),
        _ => {
            assert!(
                std::env::var("PERSISTANT_REQUIRE_TLS_DATABASE").as_deref() != Ok("1"),
                "PERSISTANT_REQUIRE_TLS_DATABASE=1 but a TLS test variable is not set"
            );
            eprintln!("skipped: the TLS test servers are not configured");
            None
        }
    }
}

fn opts(tls: TlsPolicy) -> ConnectOptions {
    ConnectOptions {
        tls,
        ..ConnectOptions::default()
    }
}

#[tokio::test]
async fn require_encrypts_and_the_server_agrees() {
    let Some(s) = servers() else { return };
    let db = PgBackend::connect_with(
        &ConnectionSecret::literal(s.tls_url),
        &opts(TlsPolicy::Require),
    )
    .await
    .unwrap();
    assert!(db.connection_is_encrypted().await.unwrap());
}

#[tokio::test]
async fn disable_is_plain_and_the_server_agrees_so_the_probe_can_say_no() {
    // Control for the probe: the same server, asked for no TLS, reports an unencrypted session.
    let Some(s) = servers() else { return };
    let db = PgBackend::connect_with(
        &ConnectionSecret::literal(s.tls_url),
        &opts(TlsPolicy::Disable),
    )
    .await
    .unwrap();
    assert!(!db.connection_is_encrypted().await.unwrap());
}

#[tokio::test]
async fn require_against_a_server_without_tls_fails_instead_of_downgrading() {
    let Some(s) = servers() else { return };
    let r = PgBackend::connect_with(
        &ConnectionSecret::literal(s.plain_url.clone()),
        &opts(TlsPolicy::Require),
    )
    .await;
    assert!(r.is_err(), "Require connected to a server with no TLS");
    // Control: Prefer, the downgrade-tolerant policy, does connect to it, unencrypted.
    let db = PgBackend::connect_with(
        &ConnectionSecret::literal(s.plain_url),
        &opts(TlsPolicy::Prefer),
    )
    .await
    .unwrap();
    assert!(!db.connection_is_encrypted().await.unwrap());
}

#[tokio::test]
async fn the_policy_overrides_a_weaker_sslmode_in_the_url() {
    let Some(s) = servers() else { return };
    let url = format!(
        "{}{}sslmode=disable",
        s.tls_url,
        if s.tls_url.contains('?') { '&' } else { '?' }
    );
    let db = PgBackend::connect_with(&ConnectionSecret::literal(url), &opts(TlsPolicy::Require))
        .await
        .unwrap();
    assert!(
        db.connection_is_encrypted().await.unwrap(),
        "the URL weakened the policy"
    );
}

#[tokio::test]
async fn verify_full_refuses_an_untrusted_certificate_and_accepts_a_trusted_one() {
    let Some(s) = servers() else { return };
    // No root given: the built-in web roots do not trust a self-signed certificate.
    let untrusted = PgBackend::connect_with(
        &ConnectionSecret::literal(s.tls_url.clone()),
        &opts(TlsPolicy::VerifyFull { root_cert: None }),
    )
    .await;
    assert!(
        matches!(
            untrusted,
            Err(RelError::Backend(_)) | Err(RelError::Unavailable(_))
        ),
        "an untrusted self-signed certificate was accepted"
    );
    // The server's own certificate as the trusted root, host `localhost` in its SAN.
    let trusted = PgBackend::connect_with(
        &ConnectionSecret::literal(s.tls_url),
        &opts(TlsPolicy::VerifyFull {
            root_cert: Some(s.ca),
        }),
    )
    .await
    .unwrap();
    assert!(trusted.connection_is_encrypted().await.unwrap());
}

#[tokio::test]
async fn an_exhausted_pool_is_an_error_after_the_bound_not_a_hang() {
    let Some(s) = servers() else { return };
    let o = ConnectOptions {
        tls: TlsPolicy::Disable,
        max_connections: 1,
        acquire_timeout: std::time::Duration::from_millis(300),
    };
    let db = PgBackend::connect_with(&ConnectionSecret::literal(s.plain_url), &o)
        .await
        .unwrap();
    use persistant_relational::RelationalBackend;
    // Hold the only connection in a transaction, then ask for another.
    let _held = db.begin().await.unwrap();
    let started = std::time::Instant::now();
    let second = db.query("SELECT 1", &[]).await;
    assert!(
        matches!(second, Err(RelError::Unavailable(_))),
        "{second:?}"
    );
    assert!(started.elapsed() < std::time::Duration::from_secs(5));
}
