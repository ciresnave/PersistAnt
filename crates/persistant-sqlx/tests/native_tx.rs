// SPDX-License-Identifier: MIT OR Apache-2.0
//! `persistant_sqlx::native` against a real PostgreSQL: read-only and snapshot transactions, commit and
//! rollback after an error and after a long gap, identifier holes and lists with hostile names, and
//! the two statement shapes Humboldt's transfer tool runs. Skipped unless the server variable is set
//! (the same rule as the other server tests).

use persistant_relational::{Isolation, RelError, RelationalBackend, RelationalTx, Value};
use persistant_sqlx::PgBackend;
use persistant_sqlx::native::{Ident, TxOptions, list, one};

async fn db() -> Option<PgBackend> {
    let Ok(url) = std::env::var("PERSISTANT_TEST_DATABASE_URL") else {
        assert!(
            std::env::var("PERSISTANT_REQUIRE_DATABASE").as_deref() != Ok("1"),
            "PERSISTANT_REQUIRE_DATABASE=1 but PERSISTANT_TEST_DATABASE_URL is not set"
        );
        eprintln!("skipped: PERSISTANT_TEST_DATABASE_URL is not set");
        return None;
    };
    Some(PgBackend::connect(&url).await.unwrap())
}

fn int(rows: &[persistant_relational::Row]) -> i64 {
    match rows[0].values[0] {
        Value::Int(n) => n,
        ref other => panic!("not an int: {other:?}"),
    }
}

/// A table of this test's own, dropped and recreated.
async fn table(db: &PgBackend, name: &str) {
    db.execute(&format!("DROP TABLE IF EXISTS \"{name}\""), &[])
        .await
        .unwrap();
    db.execute(&format!("CREATE TABLE \"{name}\" (a bigint)"), &[])
        .await
        .unwrap();
}

#[tokio::test]
async fn read_only_refuses_writes_and_read_write_does_not() {
    let Some(db) = db().await else { return };
    table(&db, "native_ro").await;
    let mut ro = db
        .begin_with(TxOptions::new(Isolation::RepeatableRead).read_only())
        .await
        .unwrap();
    let err = ro
        .execute("INSERT INTO native_ro VALUES (1)", &[])
        .await
        .unwrap_err();
    assert!(
        format!("{err}").contains("read-only"),
        "a write in a READ ONLY transaction must name that: {err}"
    );
    ro.rollback().await.unwrap();
    // control: the same statement in a read-write transaction works
    let mut rw = db
        .begin_with(TxOptions::new(Isolation::RepeatableRead))
        .await
        .unwrap();
    assert_eq!(
        rw.execute("INSERT INTO native_ro VALUES (1)", &[])
            .await
            .unwrap(),
        1
    );
    rw.rollback().await.unwrap();
}

#[tokio::test]
async fn the_snapshot_is_fixed_at_begin() {
    let Some(db) = db().await else { return };
    table(&db, "native_snap").await;
    let mut fixed = db
        .begin_with(TxOptions::new(Isolation::RepeatableRead).read_only())
        .await
        .unwrap();
    db.execute("INSERT INTO native_snap VALUES (1)", &[])
        .await
        .unwrap();
    let n = int(&fixed
        .query("SELECT count(*) FROM native_snap", &[])
        .await
        .unwrap());
    assert_eq!(n, 0, "a row committed after begin must not be visible");
    fixed.rollback().await.unwrap();
    // negative control: snapshot_now(false) takes the snapshot at the first statement, which comes
    // after the insert below, so the same sequence sees the row
    let mut late = db
        .begin_with(
            TxOptions::new(Isolation::RepeatableRead)
                .read_only()
                .snapshot_now(false),
        )
        .await
        .unwrap();
    db.execute("INSERT INTO native_snap VALUES (2)", &[])
        .await
        .unwrap();
    let n = int(&late
        .query("SELECT count(*) FROM native_snap", &[])
        .await
        .unwrap());
    assert_eq!(n, 2, "without snapshot_now the first statement fixes it");
    late.rollback().await.unwrap();
}

#[tokio::test]
async fn commit_and_rollback_work_after_an_error_and_a_long_gap() {
    let Some(db) = db().await else { return };
    table(&db, "native_end").await;
    // rollback after an error inside the transaction
    let mut tx = db
        .begin_with(TxOptions::new(Isolation::RepeatableRead))
        .await
        .unwrap();
    tx.execute("INSERT INTO native_end VALUES (1)", &[])
        .await
        .unwrap();
    tx.execute("SELECT * FROM no_such_table_native", &[])
        .await
        .unwrap_err();
    // the transaction is aborted: further statements fail, rollback must still succeed
    tx.query("SELECT 1", &[]).await.unwrap_err();
    tx.rollback().await.unwrap();
    assert_eq!(
        int(&db
            .query("SELECT count(*) FROM native_end", &[])
            .await
            .unwrap()),
        0
    );
    // commit after a gap spanning many awaits (the "verify" step)
    let mut tx = db
        .begin_with(TxOptions::new(Isolation::RepeatableRead))
        .await
        .unwrap();
    tx.execute("INSERT INTO native_end VALUES (7)", &[])
        .await
        .unwrap();
    for _ in 0..20 {
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        assert_eq!(
            int(&tx
                .query("SELECT count(*) FROM native_end", &[])
                .await
                .unwrap()),
            1
        );
    }
    tx.commit().await.unwrap();
    assert_eq!(
        int(&db
            .query("SELECT count(*) FROM native_end", &[])
            .await
            .unwrap()),
        1
    );
    // commit of an aborted transaction is a rollback server-side: the row must not appear
    let mut tx = db
        .begin_with(TxOptions::new(Isolation::RepeatableRead))
        .await
        .unwrap();
    tx.execute("INSERT INTO native_end VALUES (9)", &[])
        .await
        .unwrap();
    tx.execute("SELECT * FROM no_such_table_native", &[])
        .await
        .unwrap_err();
    let _ = tx.commit().await;
    assert_eq!(
        int(&db
            .query("SELECT count(*) FROM native_end", &[])
            .await
            .unwrap()),
        1
    );
}

#[tokio::test]
async fn deferrable_without_serializable_read_only_is_refused_before_the_server() {
    let Some(db) = db().await else { return };
    let e = db
        .begin_with(TxOptions::new(Isolation::RepeatableRead).deferrable())
        .await
        .unwrap_err();
    assert!(matches!(e, RelError::Unsupported(_)), "{e:?}");
    // control: the valid combination begins
    db.begin_with(
        TxOptions::new(Isolation::Serializable)
            .read_only()
            .deferrable(),
    )
    .await
    .unwrap()
    .rollback()
    .await
    .unwrap();
}

#[tokio::test]
async fn hostile_names_are_data_in_a_hole_and_in_a_list() {
    let Some(db) = db().await else { return };
    let hostile = [
        "a\"b",
        "with space",
        "semi;colon",
        "x\"; DROP TABLE native_h; --",
        "select",
        "UPPER",
    ];
    let cols: Vec<Ident> = hostile.iter().map(|n| Ident::new(n).unwrap()).collect();
    let t = Ident::qualified("public", "native_h").unwrap();
    let mut tx = db
        .begin_with(TxOptions::new(Isolation::ReadCommitted))
        .await
        .unwrap();
    tx.execute_ident("DROP TABLE IF EXISTS {0}", &[one(&t)], &[])
        .await
        .unwrap();
    // a column list as `{1} bigint` needs one column at a time; build the table one column per statement
    tx.execute_ident(
        "CREATE TABLE {0} ({1} bigint)",
        &[one(&t), one(&cols[0])],
        &[],
    )
    .await
    .unwrap();
    for c in &cols[1..] {
        tx.execute_ident(
            "ALTER TABLE {0} ADD COLUMN {1} bigint",
            &[one(&t), one(c)],
            &[],
        )
        .await
        .unwrap();
    }
    tx.execute_ident(
        "INSERT INTO {0} ({1}) VALUES ($1, $2, $3, $4, $5, $6)",
        &[one(&t), list(&cols)],
        &(1..=6).map(Value::Int).collect::<Vec<_>>(),
    )
    .await
    .unwrap();
    let rows = tx
        .query_ident(
            "SELECT {1} FROM {0} ORDER BY {1}",
            &[one(&t), list(&cols)],
            &[],
        )
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values.len(), 6);
    // the table the hostile name tried to drop is still here (it is this very table)
    assert_eq!(
        int(&tx
            .query_ident("SELECT count(*) FROM {0}", &[one(&t)], &[])
            .await
            .unwrap()),
        1
    );
    tx.execute_ident("DROP TABLE {0}", &[one(&t)], &[])
        .await
        .unwrap();
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn the_statement_shapes_humboldt_runs() {
    let Some(db) = db().await else { return };
    db.execute("DROP SEQUENCE IF EXISTS native_seq", &[])
        .await
        .unwrap();
    db.execute("CREATE SEQUENCE native_seq", &[]).await.unwrap();
    let mut tx = db
        .begin_with(TxOptions::new(Isolation::RepeatableRead))
        .await
        .unwrap();
    // setval with a text parameter cast to regclass, an integer and a boolean
    let rows = tx
        .query_ident(
            "SELECT setval($1::regclass, $2, $3)",
            &[],
            &[
                Value::Text("native_seq".into()),
                Value::Int(41),
                Value::Bool(true),
            ],
        )
        .await
        .unwrap();
    assert_eq!(int(&rows), 41);
    assert_eq!(
        int(&tx.query("SELECT nextval('native_seq')", &[]).await.unwrap()),
        42
    );
    // "other sessions" on this database: a second pooled connection is one
    let _other = db.pool().acquire().await.unwrap();
    let rows = tx
        .query_ident(
            "SELECT count(*) FROM pg_stat_activity WHERE datname = current_database() AND pid <> pg_backend_pid()",
            &[],
            &[],
        )
        .await
        .unwrap();
    assert!(int(&rows) >= 1, "the held connection must be counted");
    let rows = tx
        .query_ident(
            "SELECT count(*) FROM pg_stat_activity WHERE datname = $1 AND pid <> pg_backend_pid()",
            &[],
            &[Value::Text("persistant".into())],
        )
        .await
        .unwrap();
    assert!(int(&rows) >= 1);
    tx.rollback().await.unwrap();
    db.execute("DROP SEQUENCE native_seq", &[]).await.unwrap();
}
