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
use persistant::registry::{self, Endpoint, OpenError, OpenOptions};
use std::time::Duration;

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

/// Put, read back and delete one key through the handle, using a key no other test (or call) touches.
async fn roundtrip(h: &registry::Handle) {
    // One key per call: the tests of this binary run at the same time, against the same server, in one process.
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let key = format!("registry-test/{}-{n}", std::process::id());
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

/// A server opened with a call bound lists and opens `kv.bounded_call`, and without one is refused.
#[cfg(any(feature = "registry-postgres", feature = "registry-mysql"))]
#[tokio::test]
async fn a_bounded_need_opens_a_server_only_with_a_bound() {
    let id = |s| CapabilityId::new(s).unwrap();
    let bounded = ProviderNeeds::new(
        vec![
            id("kv.get"),
            id("kv.put"),
            id("kv.delete"),
            id("kv.bounded_call"),
        ],
        Durability::Volatile,
    );
    let mut cases: Vec<(&str, String)> = Vec::new();
    #[cfg(feature = "registry-postgres")]
    if let Some(v) = url_var(
        "PERSISTANT_TEST_DATABASE_URL",
        "PERSISTANT_REQUIRE_DATABASE",
    ) {
        cases.push(("postgres", v));
    }
    #[cfg(feature = "registry-mysql")]
    if let Some(v) = url_var("PERSISTANT_TEST_MYSQL_URL", "PERSISTANT_REQUIRE_MYSQL") {
        let maria = std::env::var("PERSISTANT_TEST_MYSQL_FLAVOR").as_deref() == Ok("mariadb");
        cases.push((if maria { "mariadb" } else { "mysql" }, v));
    }
    for (name, var) in cases {
        let ep = Endpoint::url_env(&var);
        let list = registry::providers(&bounded).unwrap();
        assert!(
            list.iter().any(|l| l.id == name),
            "{name} lists kv.bounded_call"
        );
        let e = registry::open_named(name, &ep, &bounded).await.unwrap_err();
        assert!(matches!(e, OpenError::BoundRequired { .. }), "{e:?}");
        let opts = OpenOptions::new().call_bound(Duration::from_secs(10));
        let h = registry::open_named_with(name, &ep, &bounded, &opts)
            .await
            .unwrap();
        assert_eq!(h.kv().call_bound(), Some(Duration::from_secs(10)));
        roundtrip(&h).await;
    }
}

/// The relational handle of each server in reach: a statement, a rolled-back transaction, and the
/// call bound carried through. Reads only (`SELECT`), so it leaves the shared server as it found it.
#[tokio::test]
async fn the_rel_handle_works_on_each_server_in_reach() {
    use persistant::relational::{Isolation, RelationalBackend, RelationalTx};

    let id = |s| CapabilityId::new(s).unwrap();
    let rel_needs = ProviderNeeds::new(
        vec![id("rel.values"), id("rel.transaction")],
        Durability::Volatile,
    );
    let mut cases: Vec<(&str, String)> = Vec::new();
    #[cfg(feature = "registry-postgres")]
    if let Some(v) = url_var(
        "PERSISTANT_TEST_DATABASE_URL",
        "PERSISTANT_REQUIRE_DATABASE",
    ) {
        cases.push(("postgres", v));
    }
    #[cfg(feature = "registry-mysql")]
    if let Some(v) = url_var("PERSISTANT_TEST_MYSQL_URL", "PERSISTANT_REQUIRE_MYSQL") {
        let maria = std::env::var("PERSISTANT_TEST_MYSQL_FLAVOR").as_deref() == Ok("mariadb");
        cases.push((if maria { "mariadb" } else { "mysql" }, v));
    }
    for (name, var) in cases {
        let ep = Endpoint::url_env(&var);
        let h = registry::open_named(name, &ep, &rel_needs).await.unwrap();
        let rel = h.rel().expect("a rel need gets a relational handle");
        assert_eq!(rel.backend(), name);
        assert_eq!(rel.query("SELECT 1 AS one", &[]).await.unwrap().len(), 1);
        let mut tx = rel.begin(Isolation::default()).await.unwrap();
        assert_eq!(tx.query("SELECT 1 AS one", &[]).await.unwrap().len(), 1);
        tx.rollback().await.unwrap();
        // Control: a kv-only need gets no relational handle.
        let k = registry::open_named(name, &ep, &needs(Durability::Volatile))
            .await
            .unwrap();
        assert!(k.rel().is_none());
        let opts = OpenOptions::new().call_bound(Duration::from_secs(10));
        let b = registry::open_named_with(name, &ep, &rel_needs, &opts)
            .await
            .unwrap();
        assert_eq!(b.rel().unwrap().call_bound(), Some(Duration::from_secs(10)));
    }
}

/// The live check at open (ROADMAP row 10) passes on each server in reach, and leaves no table behind.
#[tokio::test]
async fn a_live_check_passes_on_each_server_in_reach() {
    let mut cases: Vec<(&str, String)> = Vec::new();
    #[cfg(feature = "registry-postgres")]
    if let Some(v) = url_var(
        "PERSISTANT_TEST_DATABASE_URL",
        "PERSISTANT_REQUIRE_DATABASE",
    ) {
        cases.push(("postgres", v));
    }
    #[cfg(feature = "registry-mysql")]
    if let Some(v) = url_var("PERSISTANT_TEST_MYSQL_URL", "PERSISTANT_REQUIRE_MYSQL") {
        let maria = std::env::var("PERSISTANT_TEST_MYSQL_FLAVOR").as_deref() == Ok("mariadb");
        cases.push((if maria { "mariadb" } else { "mysql" }, v));
    }
    let opts = OpenOptions::new().with_live_check();
    for (name, var) in cases {
        let ep = Endpoint::url_env(&var);
        let h = registry::open_named_with(name, &ep, &needs(Durability::Volatile), &opts)
            .await
            .unwrap();
        roundtrip(&h).await;
    }
}

/// Control for the live check: a role without the `TEMP` privilege opens fine without the option and
/// is refused with it, naming the step. Needs a role that may create roles and databases (the CI
/// service container's superuser is one). A scratch database of its own, so no other test loses
/// `TEMP`.
#[cfg(feature = "registry-postgres")]
#[tokio::test]
async fn a_role_without_temp_is_refused_by_the_live_check_and_only_by_it() {
    use persistant::relational::RelationalBackend;
    use persistant::sqlx_driver::PgBackend;
    let Some(var) = url_var(
        "PERSISTANT_TEST_DATABASE_URL",
        "PERSISTANT_REQUIRE_DATABASE",
    ) else {
        return;
    };
    let admin_url = std::env::var(&var).unwrap();
    let (scheme, rest) = admin_url.split_once("://").unwrap();
    let (credentials, host_and_db) = rest.rsplit_once('@').unwrap();
    let (host, _db) = host_and_db.split_once('/').unwrap();
    let admin = PgBackend::connect(&admin_url).await.unwrap();
    let suffix = std::process::id();
    let db = format!("persistant_live_check_{suffix}");
    let role = format!("persistant_live_role_{suffix}");
    for sql in [
        format!("DROP DATABASE IF EXISTS {db}"),
        format!("DROP ROLE IF EXISTS {role}"),
        format!("CREATE ROLE {role} LOGIN PASSWORD 'lc-test' NOSUPERUSER"),
        format!("CREATE DATABASE {db}"),
        format!("REVOKE TEMP ON DATABASE {db} FROM PUBLIC"),
        format!("GRANT CONNECT ON DATABASE {db} TO {role}"),
    ] {
        admin.execute(&sql, &[]).await.unwrap();
    }
    // The role must be able to make the key-value tables, or the open fails for another reason.
    let inside = PgBackend::connect(&format!("{scheme}://{credentials}@{host}/{db}"))
        .await
        .unwrap();
    inside
        .execute(&format!("ALTER SCHEMA public OWNER TO {role}"), &[])
        .await
        .unwrap();
    drop(inside);
    let role_url = format!("{scheme}://{role}:lc-test@{host}/{db}");
    let ep = Endpoint::Url(role_url);
    let outcome = async {
        // Without the option: opens (the matrix is the gate), so the refusal below is the check's.
        let h = registry::open_named("postgres", &ep, &needs(Durability::Volatile))
            .await
            .unwrap();
        drop(h);
        let opts = OpenOptions::new().with_live_check();
        registry::open_named_with("postgres", &ep, &needs(Durability::Volatile), &opts)
            .await
            .err()
    }
    .await;
    for sql in [
        format!("DROP DATABASE IF EXISTS {db} WITH (FORCE)"),
        format!("DROP ROLE IF EXISTS {role}"),
    ] {
        admin.execute(&sql, &[]).await.unwrap();
    }
    match outcome {
        Some(OpenError::LiveCheck {
            id: "postgres",
            reason,
        }) => {
            assert!(reason.contains("TEMP"), "{reason}");
        }
        other => panic!("expected the live check to refuse a role without TEMP, got {other:?}"),
    }
}
