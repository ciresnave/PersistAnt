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

/// The tests share one server, and the key-value adapter keeps one table with fixed key prefixes:
/// two tests certifying at once would delete each other's keys. One at a time.
static SERVER: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

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
    let _serial = SERVER.lock().await;
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
    let _serial = SERVER.lock().await;
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
    let _serial = SERVER.lock().await;
    db.execute("DROP TABLE IF EXISTS persistant_tx", &[])
        .await
        .unwrap();
    db.execute("CREATE TABLE persistant_tx (n INT)", &[])
        .await
        .unwrap();
    {
        let mut tx = db
            .begin(persistant_relational::Isolation::ReadCommitted)
            .await
            .unwrap();
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
    let mut tx = db
        .begin(persistant_relational::Isolation::ReadCommitted)
        .await
        .unwrap();
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
    let _serial = SERVER.lock().await;
    let kv = KvOnRelational::open(db).await.unwrap();
    let rows = certify(
        &kv,
        &Config::new().with("dialect", "postgres"),
        &Config::new(),
    )
    .await;
    // kv.bounded_call is off here by design: no call timeout is configured on this backend.
    for c in &rows {
        if c.leaf == persistant_kv::conformance::Leaf::BoundedCall {
            continue;
        }
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

/// With a call timeout configured, `kv.bounded_call` is on: a batch blocked on the batch row's lock
/// by another session returns the timeout within the bound and leaves nothing applied.
#[tokio::test]
async fn kv_on_postgres_declares_bounded_call_when_a_timeout_is_set() {
    let Some(db) = backend().await else { return };
    let _serial = SERVER.lock().await;
    let bound = std::time::Duration::from_millis(3000);
    let kv = KvOnRelational::open(persistant_sqlx::Bounded::new(db, bound))
        .await
        .unwrap();
    let config = Config::new()
        .with("dialect", "postgres")
        .with("call_timeout_ms", "3000");
    let rows = persistant_kv::conformance::certify_leaves(
        &kv,
        &config,
        &Config::new(),
        &[persistant_kv::conformance::Leaf::BoundedCall],
    )
    .await;
    assert!(rows[0].row.enabled, "{}", rows[0].row.note);
}

/// Concurrent first opens of a database with no tables: `CREATE TABLE IF NOT EXISTS` races in the
/// PostgreSQL catalog, and every opener must still succeed.
#[tokio::test]
async fn concurrent_first_opens_all_succeed() {
    let Some(db) = backend().await else { return };
    let _serial = SERVER.lock().await;
    for _ in 0..5 {
        db.execute("DROP TABLE IF EXISTS persistant_kv", &[])
            .await
            .unwrap();
        db.execute("DROP TABLE IF EXISTS persistant_kv_batch", &[])
            .await
            .unwrap();
        let url = std::env::var("PERSISTANT_TEST_DATABASE_URL").unwrap();
        let opens = (0..4).map(|_| {
            let url = url.clone();
            tokio::spawn(async move {
                let db = PgBackend::connect(&url).await.expect("connect");
                KvOnRelational::open(db).await.map(|_| ())
            })
        });
        for h in opens.collect::<Vec<_>>() {
            h.await.unwrap().expect("a concurrent first open failed");
        }
    }
}

#[tokio::test]
async fn a_text_key_column_orders_by_collation_and_the_adapter_does_not() {
    // The trap, shown on this server: with a collation-aware text column "a/a" sorts before
    // "a/B"; bytewise (what `kv.ordered_scan` promises and BYTEA gives) puts "a/B" first.
    let Some(db) = backend().await else { return };
    let _serial = SERVER.lock().await;
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

/// Stage 2: the sampled-concurrency leaves, one attempt per run, five runs. One wrong run in five
/// is a flaky contract, not a pass. (Real-time leaves are left out: a stalled CI machine would fail
/// them for the wrong reason; `certify` retries those.)
#[tokio::test]
async fn the_concurrency_leaves_are_exact_on_every_run() {
    let Some(db) = backend().await else { return };
    let _serial = SERVER.lock().await;
    use persistant_kv::conformance::Leaf;
    use persistant_kv::stats::measure;
    let kv = KvOnRelational::open(db).await.expect("open");
    let leaves = [
        Leaf::PutIfAbsent,
        Leaf::CompareAndSwap,
        Leaf::CompareAndDelete,
        Leaf::Take,
        Leaf::Index,
        Leaf::BatchRollback,
        Leaf::BatchConcurrentRead,
        Leaf::OptimisticSequence,
        Leaf::AbsentRace,
    ];
    for s in measure(&kv, &leaves, 5).await {
        assert!(
            s.is_exact(),
            "{}: {}/{} runs passed, {} wrong, {} errored; first failure: {:?}",
            s.leaf.id(),
            s.passed,
            s.runs,
            s.wrong,
            s.errored,
            s.first_failure
        );
    }
}

/// The relational contract (`persistant_relational::conformance`) on postgres: every leaf on, with its
/// known-bad controls caught.
#[tokio::test]
async fn the_relational_leaves_certify_on() {
    let Some(db) = backend().await else { return };
    let _serial = SERVER.lock().await;
    let config = Config::new().with("dialect", "postgres");
    let rows = persistant_relational::conformance::certify(&db, &config, &Config::new()).await;
    let off: Vec<_> = rows
        .iter()
        .filter(|c| !c.row.enabled)
        .map(|c| format!("{}: {:?} {}", c.leaf.id(), c.row.reason, c.row.note))
        .collect();
    assert!(
        off.is_empty(),
        "relational leaves off on postgres: {off:#?}"
    );
}

/// A `NULL` parameter into a non-text column must work on every pooled connection, not only on one
/// that already prepared the statement. Eight concurrent inserts use eight connections, each preparing
/// the statement for the first time; a NULL bound as `text` fails on all but the first.
#[tokio::test]
async fn a_null_parameter_works_on_every_pooled_connection() {
    let Some(db) = backend().await else { return };
    let _serial = SERVER.lock().await;
    db.execute("DROP TABLE IF EXISTS persistant_t_null", &[])
        .await
        .unwrap();
    db.execute(
        "CREATE TABLE persistant_t_null (id BIGINT PRIMARY KEY, n BIGINT, b BYTEA)",
        &[],
    )
    .await
    .unwrap();
    let inserts = (0..8_i64).map(|i| {
        let db = &db;
        async move {
            db.execute(
                "INSERT INTO persistant_t_null (id, n, b) VALUES ($1, $2, $3)",
                &[Value::Int(i), Value::Null, Value::Null],
            )
            .await
        }
    });
    for r in futures_join_all(inserts).await {
        r.unwrap();
    }
    let n = db
        .query(
            "SELECT COUNT(*) FROM persistant_t_null WHERE n IS NULL AND b IS NULL",
            &[],
        )
        .await
        .unwrap();
    assert_eq!(n[0].values, vec![Value::Int(8)]);
    db.execute("DROP TABLE persistant_t_null", &[])
        .await
        .unwrap();
}

async fn futures_join_all<F: std::future::Future>(
    fs: impl IntoIterator<Item = F>,
) -> Vec<F::Output> {
    let handles: Vec<_> = fs.into_iter().map(Box::pin).collect();
    let mut out = Vec::new();
    // Poll them together on one task: each awaits the pool, so they overlap.
    let mut pending: Vec<std::pin::Pin<Box<F>>> = handles;
    let mut results: Vec<Option<F::Output>> = (0..pending.len()).map(|_| None).collect();
    std::future::poll_fn(|cx| {
        let mut all = true;
        for (i, f) in pending.iter_mut().enumerate() {
            if results[i].is_none() {
                match f.as_mut().poll(cx) {
                    std::task::Poll::Ready(v) => results[i] = Some(v),
                    std::task::Poll::Pending => all = false,
                }
            }
        }
        if all {
            std::task::Poll::Ready(())
        } else {
            std::task::Poll::Pending
        }
    })
    .await;
    out.extend(results.into_iter().flatten());
    out
}

/// The migration ledger (`persistant_relational::migrate_conformance`) on postgres, in a scratch schema
/// so the ledger is the test own: every leaf on, with the ledger-less migrator caught.
#[tokio::test]
async fn the_migration_leaves_certify_on() {
    let Some(admin) = backend().await else { return };
    let _serial = SERVER.lock().await;
    let url = std::env::var("PERSISTANT_TEST_DATABASE_URL").unwrap();
    admin
        .execute("DROP SCHEMA IF EXISTS persistant_mig_leaves CASCADE", &[])
        .await
        .unwrap();
    admin
        .execute("CREATE SCHEMA persistant_mig_leaves", &[])
        .await
        .unwrap();
    let sep = if url.contains('?') { '&' } else { '?' };
    let scoped = format!("{url}{sep}options=-c%20search_path%3Dpersistant_mig_leaves");
    let db = persistant_sqlx::PgBackend::connect(&scoped).await.unwrap();
    let config = Config::new().with("dialect", "postgres");
    let rows =
        persistant_relational::migrate_conformance::certify(&db, &config, &Config::new()).await;
    let off: Vec<_> = rows
        .iter()
        .filter(|c| !c.row.enabled)
        .map(|c| format!("{}: {:?} {}", c.leaf.id(), c.row.reason, c.row.note))
        .collect();
    assert!(off.is_empty(), "migration leaves off on postgres: {off:#?}");
    assert_eq!(
        rows.len(),
        persistant_relational::migrate_conformance::MigrateLeaf::ALL.len()
    );

    // A failed migration must not wedge the next run. sqlx holds a session advisory lock for a run and does
    // not release it on failure; without ending that session the next run, on another pooled connection,
    // waited for it forever (measured on MySQL 8.4: 550 s and counting).
    use persistant_relational::{MigrateError, Migration};
    for t in ["persistant_wedge_t", "_sqlx_migrations"] {
        db.execute(&format!("DROP TABLE IF EXISTS {t}"), &[])
            .await
            .unwrap();
    }
    let bad = [
        Migration::new(
            1,
            "t",
            "CREATE TABLE persistant_wedge_t (id BIGINT PRIMARY KEY)",
        ),
        Migration::new(
            2,
            "bad",
            "INSERT INTO persistant_wedge_t (id) VALUES (1); INSERT INTO persistant_wedge_t (id) VALUES (1)",
        ),
    ];
    assert!(matches!(
        db.migrate(&bad).await,
        Err(MigrateError::Failed { version: 2, .. })
    ));
    let good = [
        bad[0].clone(),
        Migration::new(2, "good", "INSERT INTO persistant_wedge_t (id) VALUES (1)"),
    ];
    let next = tokio::time::timeout(std::time::Duration::from_secs(30), db.migrate(&good))
        .await
        .expect("the run after a failed migration waited on a lock the failed run left behind");
    assert_eq!(next.unwrap().applied, vec![2]);
    for t in ["persistant_wedge_t", "_sqlx_migrations"] {
        db.execute(&format!("DROP TABLE IF EXISTS {t}"), &[])
            .await
            .unwrap();
    }
}
