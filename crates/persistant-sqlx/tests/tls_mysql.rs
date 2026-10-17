// SPDX-License-Identifier: MIT OR Apache-2.0
//! TLS policy against a real MySQL 8.4 that offers TLS with its auto-generated private CA, and one
//! started with `--skip-ssl`.
//!
//! Needs `PERSISTANT_TEST_MYSQL_TLS_URL` (the TLS server), `PERSISTANT_TEST_MYSQL_TLS_CA` (its CA,
//! PEM) and `PERSISTANT_TEST_MYSQL_PLAIN_URL` (the server without TLS). Skipped without them
//! unless `PERSISTANT_REQUIRE_MYSQL_TLS=1` (set by the CI job that starts both servers), in which
//! case a missing variable is a failure.

use persistant_relational::{ConnectionSecret, RelError};
use persistant_sqlx::{ConnectOptions, MysqlBackend, TlsPolicy};
use std::path::PathBuf;

struct Servers {
    tls_url: String,
    ca: PathBuf,
    plain_url: String,
}

fn servers() -> Option<Servers> {
    let get = |k: &str| std::env::var(k).ok();
    match (
        get("PERSISTANT_TEST_MYSQL_TLS_URL"),
        get("PERSISTANT_TEST_MYSQL_TLS_CA"),
        get("PERSISTANT_TEST_MYSQL_PLAIN_URL"),
    ) {
        (Some(tls_url), Some(ca), Some(plain_url)) => Some(Servers {
            tls_url,
            ca: PathBuf::from(ca),
            plain_url,
        }),
        _ => {
            assert!(
                std::env::var("PERSISTANT_REQUIRE_MYSQL_TLS").as_deref() != Ok("1"),
                "PERSISTANT_REQUIRE_MYSQL_TLS=1 but a MySQL TLS test variable is not set"
            );
            eprintln!("skipped: the MySQL TLS test servers are not configured");
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
    let db = MysqlBackend::connect_with(
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
    let db = MysqlBackend::connect_with(
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
    let r = MysqlBackend::connect_with(
        &ConnectionSecret::literal(s.plain_url.clone()),
        &opts(TlsPolicy::Require),
    )
    .await;
    assert!(r.is_err(), "Require connected to a server with no TLS");
    // Control: Prefer, the downgrade-tolerant policy, does connect to it, unencrypted.
    let db = MysqlBackend::connect_with(
        &ConnectionSecret::literal(s.plain_url),
        &opts(TlsPolicy::Prefer),
    )
    .await
    .unwrap();
    assert!(!db.connection_is_encrypted().await.unwrap());
}

#[tokio::test]
async fn the_policy_overrides_a_weaker_ssl_mode_in_the_url() {
    let Some(s) = servers() else { return };
    let url = format!(
        "{}{}ssl-mode=disabled",
        s.tls_url,
        if s.tls_url.contains('?') { '&' } else { '?' }
    );
    let db = MysqlBackend::connect_with(&ConnectionSecret::literal(url), &opts(TlsPolicy::Require))
        .await
        .unwrap();
    assert!(
        db.connection_is_encrypted().await.unwrap(),
        "the URL weakened the policy"
    );
}

#[tokio::test]
async fn verify_ca_refuses_an_untrusted_certificate_and_accepts_a_trusted_one() {
    let Some(s) = servers() else { return };
    // No root given: the built-in web roots do not trust the server's private CA.
    let untrusted = MysqlBackend::connect_with(
        &ConnectionSecret::literal(s.tls_url.clone()),
        &opts(TlsPolicy::VerifyCa { root_cert: None }),
    )
    .await;
    assert!(
        matches!(
            untrusted,
            Err(RelError::Backend(_)) | Err(RelError::Unavailable(_))
        ),
        "an untrusted certificate was accepted"
    );
    let trusted = MysqlBackend::connect_with(
        &ConnectionSecret::literal(s.tls_url),
        &opts(TlsPolicy::VerifyCa {
            root_cert: Some(s.ca),
        }),
    )
    .await
    .unwrap();
    assert!(trusted.connection_is_encrypted().await.unwrap());
}

#[tokio::test]
async fn verify_full_refuses_a_certificate_that_does_not_name_the_host() {
    // MySQL's auto-generated certificate chains to its CA but names no host: VerifyCa accepts it
    // (the test above), VerifyFull must not.
    let Some(s) = servers() else { return };
    let r = MysqlBackend::connect_with(
        &ConnectionSecret::literal(s.tls_url),
        &opts(TlsPolicy::VerifyFull {
            root_cert: Some(s.ca),
        }),
    )
    .await;
    assert!(
        matches!(r, Err(RelError::Backend(_)) | Err(RelError::Unavailable(_))),
        "VerifyFull accepted a certificate that does not name the host"
    );
}
