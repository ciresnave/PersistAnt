// SPDX-License-Identifier: MIT OR Apache-2.0
//! The registry against real servers: the id kept in config opens the server, and a server that is
//! not the one the id names is refused.
//!
//! Needs `PERSISTANT_TEST_DATABASE_URL` (PostgreSQL) or `PERSISTANT_TEST_MYSQL_URL` (MySQL or, with
//! `PERSISTANT_TEST_MYSQL_FLAVOR=mariadb`, MariaDB). Without it these tests print a note and pass,
//! **unless** the matching `PERSISTANT_REQUIRE_DATABASE` / `PERSISTANT_REQUIRE_MYSQL` is `1` (set by
//! the CI jobs that provide a server): a skipped test must not look like a green one there.
#![cfg(any(feature = "registry-postgres", feature = "registry-mysql"))]

use persistant::capability::CapabilityId;
use persistant::capability::choose::Durability;
use persistant::capability::provider::ProviderNeeds;
use persistant::kv::KvBackend;
use persistant::registry::{self, Endpoint, OpenError};

fn needs(min: Durability) -> ProviderNeeds {
    let id = |s| CapabilityId::new(s).unwrap();
    ProviderNeeds::new(vec![id("kv.get"), id("kv.put"), id("kv.delete")], min)
}

/// The environment variable holding the URL, or `None` (skipped) when it is unset and not required.
fn url_var(var: &str, require: &str) -> Option<String> {
    match std::env::var(var) {
        Ok(_) => Some(var.to_string()),
        Err(_) => {
            assert!(
                std::env::var(require).as_deref() != Ok("1"),
                "{require}=1 but {var} is not set"
            );
            eprintln!("skipped: {var} is not set");
            None
        }
    }
}

/// Put, read back and delete one key through the handle, using a key no other test touches.
async fn roundtrip(h: &registry::Handle) {
    let key = format!("registry-test/{}", std::process::id());
    h.kv().put(&key, b"v".to_vec(), None).await.unwrap();
    assert_eq!(h.kv().get(&key).await.unwrap(), Some(b"v".to_vec()));
    h.kv().delete(&key).await.unwrap();
    assert_eq!(h.kv().get(&key).await.unwrap(), None);
}

#[cfg(feature = "registry-postgres")]
#[tokio::test]
async fn postgres_is_listed_with_its_text_and_opens_from_a_secret_url() {
    let Some(var) = url_var(
        "PERSISTANT_TEST_DATABASE_URL",
        "PERSISTANT_REQUIRE_DATABASE",
    ) else {
        return;
    };
    let list = registry::providers(&needs(Durability::Machine)).unwrap();
    let pg = list
        .iter()
        .find(|l| l.id == "postgres")
        .expect("postgres is listed for Machine");
    let why = pg.why().join("\n");
    assert!(
        why.contains("durability: machine (verified: machine)"),
        "{why}"
    );
    assert!(why.contains("recorded on version"), "{why}");
    assert!(why.contains("full disk fails loud"), "{why}");

    let ep = Endpoint::url_env(&var);
    let h = registry::open_named("postgres", &ep, &needs(Durability::Machine))
        .await
        .unwrap();
    assert_eq!(h.kv().backend(), "kv-on-relational:postgres");
    roundtrip(&h).await;
    // The Debug text of the endpoint names the variable, never the URL.
    let dbg = format!("{ep:?}");
    assert!(
        dbg.contains(&var) && !dbg.contains("persistant-test"),
        "{dbg}"
    );

    // A URL of the wrong scheme is refused before any connection.
    let e = registry::open_named(
        "postgres",
        &Endpoint::Url("mysql://x@127.0.0.1/y".into()),
        &needs(Durability::Volatile),
    )
    .await
    .unwrap_err();
    assert!(
        matches!(
            e,
            OpenError::WrongEndpoint {
                id: "postgres",
                got: "url",
                ..
            }
        ),
        "{e:?}"
    );
}

#[cfg(feature = "registry-mysql")]
#[tokio::test]
async fn mysql_and_mariadb_are_told_apart_and_the_wrong_one_is_refused() {
    let Some(var) = url_var("PERSISTANT_TEST_MYSQL_URL", "PERSISTANT_REQUIRE_MYSQL") else {
        return;
    };
    let maria = std::env::var("PERSISTANT_TEST_MYSQL_FLAVOR").as_deref() == Ok("mariadb");
    let (right, wrong) = if maria {
        ("mariadb", "mysql")
    } else {
        ("mysql", "mariadb")
    };
    let ep = Endpoint::url_env(&var);

    let list = registry::providers(&needs(Durability::Volatile)).unwrap();
    let text = list
        .iter()
        .find(|l| l.id == "mysql")
        .unwrap()
        .why()
        .join("\n");
    assert!(text.contains("STALL"), "{text}");
    assert!(text.contains("durability: machine"), "{text}");

    let h = registry::open_named(right, &ep, &needs(Durability::Volatile))
        .await
        .unwrap();
    assert_eq!(h.kv().backend(), format!("kv-on-relational:{right}"));
    roundtrip(&h).await;

    let e = registry::open_named(wrong, &ep, &needs(Durability::Volatile))
        .await
        .unwrap_err();
    assert!(
        matches!(&e, OpenError::ServerMismatch { id, found } if *id == wrong && found.ends_with(right)),
        "{e:?}"
    );
}
