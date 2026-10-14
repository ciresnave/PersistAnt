// SPDX-License-Identifier: MIT OR Apache-2.0
//! The relational driver and `KvOnRelational` against a real PostgreSQL.
//!
//! Needs `PERSISTANT_TEST_DATABASE_URL`. Without it these tests print a note and pass, **unless**
//! `PERSISTANT_REQUIRE_DATABASE=1` (set by the CI job that provides a server), in which case a
//! missing URL is a failure: a skipped test must not look like a green one there.

use persistant_core::Config;
use persistant_kv::conformance::{Certified, certify};
use persistant_kv::{KvBackend, KvOnRelational};
use persistant_relational::{RelError, RelationalBackend, RelationalTx, Value};
use persistant_sqlx::PgBackend;

async fn backend() -> Option<PgBackend> {
    match std::env::var("PERSISTANT_TEST_DATABASE_URL") {
        Ok(url) => Some(PgBackend::connect(&url).await.expect("connect")),
        Err(_) => {
            assert!(
                std::env::var("PERSISTANT_REQUIRE_DATABASE").as_deref() != Ok("1"),
                "PERSISTANT_REQUIRE_DATABASE=1 but PERSISTANT_TEST_DATABASE_URL is not set"
            );
            eprintln!("skipped: PERSISTANT_TEST_DATABASE_URL is not set");
            None
        }
    }
}

fn summary(rows: &[Certified]) -> String {
    rows.iter()
        .map(|c| {
            format!(
                "{}={}{}",
                c.leaf.id(),
                if c.row.enabled { "on" } else { "off" },
                c.row.reason.map(|r| format!("({r})")).unwrap_or_default()
            )
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[tokio::test]
async fn statements_parameters_and_values_round_trip() {
    let Some(db) = backend().await else { return };
    assert_eq!(db.backend(), "postgres");
    assert!(
        db.version()
            .split('.')
            .next()
            .unwrap()
            .parse::<u32>()
            .is_ok(),
        "version {:?}",
        db.version()
    );
    let rows = db
        .query(
            "SELECT $1::bigint, $2::text, $3::bytea, $4::boolean, $5::double precision",
            &[
                Value::Int(-7),
                Value::Text("ünï".into()),
                Value::Bytes(vec![0, 255, 1]),
                Value::Bool(true),
                Value::Float(1.5),
            ],
        )
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].values,
        vec![
            Value::Int(-7),
            Value::Text("ünï".into()),
            Value::Bytes(vec![0, 255, 1]),
            Value::Bool(true),
            Value::Float(1.5)
        ]
    );
}

#[tokio::test]
async fn a_connection_secret_connects_and_the_backend_never_prints_the_password() {
    use persistant_relational::ConnectionSecret;
    let Ok(url) = std::env::var("PERSISTANT_TEST_DATABASE_URL") else {
        assert!(
            std::env::var("PERSISTANT_REQUIRE_DATABASE").as_deref() != Ok("1"),
            "PERSISTANT_REQUIRE_DATABASE=1 but PERSISTANT_TEST_DATABASE_URL is not set"
        );
        eprintln!("skipped: PERSISTANT_TEST_DATABASE_URL is not set");
        return;
    };
    // The password of the URL under test, if it has one.
    let password = url
        .split_once("://")
        .and_then(|(_, rest)| rest.split_once('@'))
        .and_then(|(userinfo, _)| userinfo.split_once(':'))
        .map(|(_, p)| p.to_string());
    let secret = ConnectionSecret::literal(url);
    let db = PgBackend::connect_secret(&secret).await.expect("connect");
    assert_eq!(
        db.query("SELECT 1::bigint", &[]).await.unwrap()[0].int(0),
        Some(1)
    );
    if let Some(p) = password.filter(|p| p.len() >= 4) {
        let shown = format!("{secret:?} {db:?}");
        assert!(!shown.contains(&p), "a Debug print contained the password");
    }
    // Control: an unset variable fails by name and prints no value.
    let missing = ConnectionSecret::Env("PERSISTANT_TEST_NO_SUCH_VARIABLE".into());
    let err = PgBackend::connect_secret(&missing)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("PERSISTANT_TEST_NO_SUCH_VARIABLE"), "{err}");
}

#[tokio::test]
async fn errors_are_classified_by_sqlstate() {
    let Some(db) = backend().await else { return };
    db.execute("DROP TABLE IF EXISTS persistant_errs", &[])
        .await
        .unwrap();
    db.execute(
        "CREATE TABLE persistant_errs (id INT PRIMARY KEY, n INT CHECK (n > 0))",
        &[],
    )
    .await
    .unwrap();
    db.execute("INSERT INTO persistant_errs VALUES (1, 1)", &[])
        .await
        .unwrap();
    let dup = db
        .execute("INSERT INTO persistant_errs VALUES (1, 1)", &[])
        .await;
    assert!(
        matches!(&dup, Err(RelError::UniqueViolation { constraint: Some(c) }) if c == "persistant_errs_pkey"),
        "{dup:?}"
    );
    let chk = db
        .execute("INSERT INTO persistant_errs VALUES (2, 0)", &[])
        .await;
    assert!(
        matches!(chk, Err(RelError::CheckViolation { .. })),
        "{chk:?}"
    );
    let syn = db.execute("SELEKT 1", &[]).await;
    assert!(matches!(syn, Err(RelError::Backend(_))), "{syn:?}");
    db.execute("DROP TABLE persistant_errs", &[]).await.unwrap();
}

#[tokio::test]
async fn a_dropped_transaction_rolls_back_and_a_committed_one_persists() {
    let Some(db) = backend().await else { return };
    db.execute("DROP TABLE IF EXISTS persistant_tx", &[])
        .await
        .unwrap();
    db.execute("CREATE TABLE persistant_tx (n INT)", &[])
        .await
        .unwrap();
    {
        let mut tx = db.begin().await.unwrap();
        tx.execute("INSERT INTO persistant_tx VALUES (1)", &[])
            .await
            .unwrap();
        // dropped without commit
    }
    let n = db
        .query("SELECT count(*) FROM persistant_tx", &[])
        .await
        .unwrap();
    assert_eq!(n[0].int(0), Some(0), "dropped transaction must roll back");
    let mut tx = db.begin().await.unwrap();
    tx.execute("INSERT INTO persistant_tx VALUES (2)", &[])
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let n = db
        .query("SELECT count(*) FROM persistant_tx", &[])
        .await
        .unwrap();
    assert_eq!(n[0].int(0), Some(1));
    db.execute("DROP TABLE persistant_tx", &[]).await.unwrap();
}

#[tokio::test]
async fn kv_on_postgres_gives_every_certified_leaf() {
    let Some(db) = backend().await else { return };
    let kv = KvOnRelational::open(db).await.unwrap();
    let rows = certify(
        &kv,
        &Config::new().with("dialect", "postgres"),
        &Config::new(),
    )
    .await;
    for c in &rows {
        assert!(c.row.enabled, "{}: {}", c.leaf.id(), summary(&rows));
    }
    assert_eq!(rows[0].row.backend, "kv-on-relational:postgres");
    // The same gate reads the rows it produced.
    let m = persistant_core::Matrix::from_rows(rows.iter().map(|c| c.row.clone()).collect());
    let id = persistant_core::CapabilityId::new("lock.lease").unwrap();
    assert!(m.enabled(
        &id,
        kv.backend(),
        kv.version(),
        &Config::new().with("dialect", "postgres")
    ));
}

#[tokio::test]
async fn a_text_key_column_orders_by_collation_and_the_adapter_does_not() {
    // The trap, shown on this server: with a collation-aware text column "a/a" sorts before
    // "a/B"; bytewise (what `kv.ordered_scan` promises and BYTEA gives) puts "a/B" first.
    let Some(db) = backend().await else { return };
    let text = db
        .query(
            "SELECT s FROM (VALUES ('a/a'), ('a/B')) AS t(s) ORDER BY s COLLATE \"en_US\"",
            &[],
        )
        .await;
    let Ok(text) = text else {
        eprintln!("skipped: the server has no en_US collation");
        return;
    };
    assert_eq!(text[0].text(0), Some("a/a"), "en_US puts a/a before a/B");
    let bytes = db
        .query(
            "SELECT s FROM (VALUES ('a/a'::bytea), ('a/B'::bytea)) AS t(s) ORDER BY s",
            &[],
        )
        .await
        .unwrap();
    assert_eq!(
        bytes[0].bytes(0),
        Some(&b"a/B"[..]),
        "bytea orders bytewise"
    );
}
