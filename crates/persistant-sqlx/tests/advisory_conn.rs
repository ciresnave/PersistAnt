// SPDX-License-Identifier: MIT OR Apache-2.0
//! Named transaction locks on an sqlx connection the program already holds (`persistant_sqlx::advisory`):
//! a held name refuses another connection until the transaction ends, a different name is free, and the
//! 32-bit (`hashtext`) and 64-bit (`hashtextextended`) keys are different locks. PostgreSQL only; skipped
//! unless its server variable is set (the same rule as the other server tests).

use persistant_sqlx::advisory;

#[tokio::test]
async fn a_lock_on_an_sqlx_transaction_holds_until_it_ends() {
    let Ok(url) = std::env::var("PERSISTANT_TEST_DATABASE_URL") else {
        assert!(
            std::env::var("PERSISTANT_REQUIRE_DATABASE").as_deref() != Ok("1"),
            "PERSISTANT_REQUIRE_DATABASE=1 but PERSISTANT_TEST_DATABASE_URL is not set"
        );
        eprintln!("skipped: PERSISTANT_TEST_DATABASE_URL is not set");
        return;
    };
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(4)
        .connect(&url)
        .await
        .unwrap();
    const NAME: &str = "persistant-sqlx/advisory-conn/held";
    const OTHER: &str = "persistant-sqlx/advisory-conn/other";

    // 32-bit key.
    let mut holder = pool.begin().await.unwrap();
    advisory::lock(&mut holder, NAME).await.unwrap();
    // Taking it again in the same transaction returns at once.
    advisory::lock(&mut holder, NAME).await.unwrap();
    let mut probe = pool.begin().await.unwrap();
    assert!(!advisory::try_lock(&mut probe, NAME).await.unwrap());
    assert!(advisory::try_lock(&mut probe, OTHER).await.unwrap());
    // The 64-bit key for the same name is a different lock: not refused.
    assert!(advisory::try_lock64(&mut probe, NAME).await.unwrap());
    probe.rollback().await.unwrap();
    holder.commit().await.unwrap();
    let mut probe = pool.begin().await.unwrap();
    assert!(advisory::try_lock(&mut probe, NAME).await.unwrap());
    probe.rollback().await.unwrap();

    // 64-bit key: held, then released by a rollback.
    let mut holder = pool.begin().await.unwrap();
    advisory::lock64(&mut holder, NAME).await.unwrap();
    let mut probe = pool.begin().await.unwrap();
    assert!(!advisory::try_lock64(&mut probe, NAME).await.unwrap());
    assert!(advisory::try_lock64(&mut probe, OTHER).await.unwrap());
    // The 32-bit key for the same name is free while the 64-bit one is held.
    assert!(advisory::try_lock(&mut probe, NAME).await.unwrap());
    probe.rollback().await.unwrap();
    holder.rollback().await.unwrap();
    let mut probe = pool.begin().await.unwrap();
    assert!(advisory::try_lock64(&mut probe, NAME).await.unwrap());
    probe.rollback().await.unwrap();
}

/// The `RelationalTx` variants (`persistant_relational::advisory`) take the same 64-bit key as the
/// connection ones, so the two kinds of caller contend.
#[tokio::test]
async fn the_relational_64_bit_lock_contends_with_the_connection_one() {
    use persistant_relational::{Dialect, Isolation, RelationalBackend, RelationalTx};
    let Ok(url) = std::env::var("PERSISTANT_TEST_DATABASE_URL") else {
        assert!(
            std::env::var("PERSISTANT_REQUIRE_DATABASE").as_deref() != Ok("1"),
            "PERSISTANT_REQUIRE_DATABASE=1 but PERSISTANT_TEST_DATABASE_URL is not set"
        );
        eprintln!("skipped: PERSISTANT_TEST_DATABASE_URL is not set");
        return;
    };
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(4)
        .connect(&url)
        .await
        .unwrap();
    let db = persistant_sqlx::PgBackend::from_pool(pool.clone())
        .await
        .unwrap();
    const NAME: &str = "persistant-sqlx/advisory-conn/relational";
    let mut holder = db.begin(Isolation::ReadCommitted).await.unwrap();
    persistant_relational::advisory::lock64(&mut holder, &Dialect::Postgres, NAME)
        .await
        .unwrap();
    let mut probe = pool.begin().await.unwrap();
    assert!(!advisory::try_lock64(&mut probe, NAME).await.unwrap());
    assert!(advisory::try_lock(&mut probe, NAME).await.unwrap());
    probe.rollback().await.unwrap();
    holder.commit().await.unwrap();
}
