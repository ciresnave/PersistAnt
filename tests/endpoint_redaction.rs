// SPDX-License-Identifier: MIT OR Apache-2.0
//! `Endpoint::Url` holds a connection URL as plain text, and a URL can carry a password in its
//! userinfo (`user:password@host`) or a token in its query. A `Debug` print of the endpoint, and of
//! anything that holds one, must show neither (PM ruling 2026-11-01: a bug, not a design choice).
#![cfg(feature = "registry")]

use persistant::registry::Endpoint;

const URL: &str = "postgres://app:hunter2pass@db.example:5432/prod?password=qq9secret&sslmode=require&token=tok_abc#frag9secret";

fn shown(e: &Endpoint) -> [String; 2] {
    [format!("{e:?}"), format!("{e:#?}")]
}

#[test]
fn debug_of_a_credentialed_url_hides_password_and_query_values() {
    for text in shown(&Endpoint::Url(URL.into())) {
        for secret in ["hunter2pass", "qq9secret", "tok_abc", "frag9secret"] {
            assert!(!text.contains(secret), "{secret} leaked: {text}");
        }
    }
}

#[test]
fn debug_still_says_where_and_which_options() {
    let [text, _] = shown(&Endpoint::Url(URL.into()));
    for kept in ["postgres", "db.example:5432", "/prod", "sslmode", "Url"] {
        assert!(text.contains(kept), "{kept} missing: {text}");
    }
}

#[test]
fn a_password_with_an_at_sign_or_slash_is_still_hidden() {
    let e = Endpoint::Url("mysql://u:p@ss/word9x@host/db".into());
    for text in shown(&e) {
        assert!(!text.contains("ss/word9x"), "leaked: {text}");
        assert!(text.contains("host/db"), "{text}");
    }
}

#[test]
fn text_that_is_not_a_url_is_hidden_whole() {
    let e = Endpoint::Url("user=app password=hunter2pass host=db".into());
    for text in shown(&e) {
        assert!(!text.contains("hunter2pass"), "leaked: {text}");
    }
}

#[test]
fn a_url_without_credentials_is_unchanged_and_equality_is_by_value() {
    let plain = "sqlite:///var/data/app.db";
    let [text, _] = shown(&Endpoint::Url(plain.into()));
    assert!(text.contains(plain), "{text}");
    assert_eq!(Endpoint::Url(URL.into()), Endpoint::Url(URL.into()));
    assert_ne!(Endpoint::Url(URL.into()), Endpoint::Url(plain.into()));
}

#[test]
fn other_variants_still_print() {
    assert_eq!(format!("{:?}", Endpoint::None), "None");
    assert!(format!("{:?}", Endpoint::url_env("DB_URL")).contains("DB_URL"));
}
