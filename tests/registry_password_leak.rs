// SPDX-License-Identifier: MIT OR Apache-2.0
//! `registry::open_named` must never put the PASSWORD of its connection string in the error it
//! returns: not in `Display`, not in `Debug` (plain or `{:#?}`), not in any `source()`. Tried with a
//! canary password against a port nothing listens on and against text that is not a connection
//! string, through every way an endpoint can hold the URL (`Url`, `Secret` literal, `url_env`).
//! Row 24 of `ROADMAP.md`; the same method as `crates/persistant-sqlx/tests/connect_error_leak.rs`.
//!
//! Not covered here: a real server given the wrong password (needs the CI servers).
#![cfg(any(feature = "registry-postgres", feature = "registry-mysql"))]
#![allow(
    deprecated,
    reason = "`Endpoint::Url` is deprecated (0.17.0); this test still covers it"
)]

use persistant::capability::CapabilityId;
use persistant::capability::choose::Durability;
use persistant::capability::provider::ProviderNeeds;
use persistant::registry::{self, Endpoint, OpenError};
use std::error::Error;

/// Not a real password. Distinctive enough that its presence in text is not chance.
const CANARY: &str = "Cnry9x7Qz4Lw2Vb8";

fn needs() -> ProviderNeeds {
    let id = |s| CapabilityId::new(s).unwrap();
    ProviderNeeds::new(vec![id("kv.get"), id("kv.put")], Durability::Volatile)
}

/// Everything an error can show.
fn everything(e: &OpenError) -> String {
    let mut out = format!("{e}\n{e:?}\n{e:#?}");
    let mut src = e.source();
    while let Some(s) = src {
        out.push_str(&format!("\n{s}\n{s:?}"));
        src = s.source();
    }
    out
}

/// The endpoints holding `url`, by every route.
fn endpoints(url: &str, var: &str) -> Vec<(&'static str, Endpoint)> {
    // SAFETY-free: a unique variable name per call site, set before the one open that reads it.
    unsafe { std::env::set_var(var, url) };
    vec![
        ("Url", Endpoint::Url(url.to_string())),
        (
            "Secret::literal",
            Endpoint::Secret(persistant::relational::ConnectionSecret::literal(url)),
        ),
        ("url_env", Endpoint::url_env(var)),
    ]
}

async fn assert_open_is_clean(id: &str, url: &str, var: &str) {
    for (route, ep) in endpoints(url, var) {
        match registry::open_named(id, &ep, &needs()).await {
            Ok(_) => panic!("{id} via {route}: opened {url:.14}.. , must fail"),
            Err(e) => {
                let text = everything(&e);
                eprintln!("{id} via {route}: {e}");
                assert!(
                    !text.contains(CANARY),
                    "{id} via {route}: the password leaked: {text}"
                );
            }
        }
    }
}

/// Positive control: the scanner sees the canary when it is there.
#[test]
fn the_scan_finds_a_canary_that_is_present() {
    let e = OpenError::Open(format!("failed with {CANARY}"));
    assert!(everything(&e).contains(CANARY));
}

#[cfg(feature = "registry-postgres")]
#[tokio::test]
async fn postgres_open_errors_do_not_show_the_password() {
    // Nothing listens on port 1.
    assert_open_is_clean(
        "postgres",
        &format!("postgres://app:{CANARY}@127.0.0.1:1/db?application_name=x"),
        "PERSISTANT_LEAK_PG_CLOSED",
    )
    .await;
    for (i, bad) in [
        format!("postgres://app:{CANARY}@127.0.0.1:notaport/db"),
        format!("host=127.0.0.1 user=app password={CANARY}"),
        format!("postgres://app:{CANARY}@[::1/db"),
    ]
    .iter()
    .enumerate()
    {
        assert_open_is_clean("postgres", bad, &format!("PERSISTANT_LEAK_PG_BAD{i}")).await;
    }
}

#[cfg(feature = "registry-mysql")]
#[tokio::test]
async fn mysql_and_mariadb_open_errors_do_not_show_the_password() {
    for id in ["mysql", "mariadb"] {
        assert_open_is_clean(
            id,
            &format!("mysql://app:{CANARY}@127.0.0.1:1/db"),
            &format!("PERSISTANT_LEAK_{id}_CLOSED"),
        )
        .await;
        for (i, bad) in [
            format!("mysql://app:{CANARY}@127.0.0.1:notaport/db"),
            format!("mysql://app:{CANARY}@[::1/db"),
        ]
        .iter()
        .enumerate()
        {
            assert_open_is_clean(id, bad, &format!("PERSISTANT_LEAK_{id}_BAD{i}")).await;
        }
    }
}
