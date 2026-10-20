// SPDX-License-Identifier: MIT OR Apache-2.0
//! `persistant::sql!` end to end: a statement checked and translated at build time, run by each
//! engine's own text. SQLite always; PostgreSQL and MySQL when their server variables are set (the
//! same rule as the other server tests).

use persistant_macros::sql;
use persistant_relational::{
    Isolation, RelationalBackend, RelationalSqlExt, RelationalTx, RelationalTxSqlExt, Sql, Value,
};

const INSERT: Sql = sql!(
    crate = ::persistant_relational::embed,
    source = postgres,
    targets = [postgres, sqlite, mysql, mariadb],
    "INSERT INTO persistant_sqlmacro (id, name) VALUES ($1, $2)"
);

// $1 is used twice: SQLite numbers its placeholders, MySQL repeats positional ones. With no schema
// the macro cannot know `id` is a number, so ordering and comparing it needs the allowance named
// (the build refuses without it: that is the point of the check).
const FIND: Sql = sql!(
    crate = ::persistant_relational::embed,
    source = postgres,
    targets = [postgres, sqlite, mysql, mariadb],
    allow = [text_collation],
    "SELECT name FROM persistant_sqlmacro WHERE id = $1 OR id = $1 + 1 ORDER BY id"
);

async fn run<B: RelationalBackend>(db: &B) {
    db.execute("DROP TABLE IF EXISTS persistant_sqlmacro", &[])
        .await
        .unwrap();
    db.execute(
        "CREATE TABLE persistant_sqlmacro (id BIGINT PRIMARY KEY, name VARCHAR(40) NOT NULL)",
        &[],
    )
    .await
    .unwrap();
    for (id, name) in [(1, "one"), (2, "two"), (5, "five")] {
        let n = db
            .execute_sql(&INSERT, &[Value::Int(id), Value::Text(name.into())])
            .await
            .unwrap();
        assert_eq!(n, 1);
    }
    let rows = db.query_sql(&FIND, &[Value::Int(1)]).await.unwrap();
    let names: Vec<_> = rows
        .iter()
        .map(|r| r.text(0).unwrap().to_string())
        .collect();
    assert_eq!(names, ["one", "two"], "on {}", db.backend());

    // The same statement inside a transaction; rolled back, so nothing of it is left.
    let dialect = db.dialect();
    let mut tx = db.begin(Isolation::ReadCommitted).await.unwrap();
    tx.execute_sql(
        &dialect,
        &INSERT,
        &[Value::Int(9), Value::Text("nine".into())],
    )
    .await
    .unwrap();
    let inside = tx
        .query_sql(&dialect, &FIND, &[Value::Int(8)])
        .await
        .unwrap();
    assert_eq!(inside.len(), 1, "the transaction sees its own insert");
    tx.rollback().await.unwrap();
    assert!(
        db.query_sql(&FIND, &[Value::Int(8)])
            .await
            .unwrap()
            .is_empty()
    );

    // The wrong number of parameters never reaches the server.
    assert!(db.query_sql(&FIND, &[]).await.is_err());
    db.execute("DROP TABLE persistant_sqlmacro", &[])
        .await
        .unwrap();
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn on_sqlite() {
    let path = std::env::temp_dir().join(format!("persistant-sqlmacro-{}.db", std::process::id()));
    let db = persistant_sqlx::SqliteBackend::open(&path).await.unwrap();
    run(&db).await;
    drop(db);
    for ext in ["", "-wal", "-shm"] {
        let mut p = path.as_os_str().to_owned();
        p.push(ext);
        let _ = std::fs::remove_file(p);
    }
}

/// The tests share each server and one table name: one at a time per server.
static PG: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[tokio::test]
async fn on_postgres() {
    let Ok(url) = std::env::var("PERSISTANT_TEST_DATABASE_URL") else {
        assert!(
            std::env::var("PERSISTANT_REQUIRE_DATABASE").as_deref() != Ok("1"),
            "PERSISTANT_REQUIRE_DATABASE=1 but PERSISTANT_TEST_DATABASE_URL is not set"
        );
        eprintln!("skipped: PERSISTANT_TEST_DATABASE_URL is not set");
        return;
    };
    let _serial = PG.lock().await;
    run(&persistant_sqlx::PgBackend::connect(&url).await.unwrap()).await;
}

#[cfg(feature = "mysql")]
#[tokio::test]
async fn on_mysql() {
    let Ok(url) = std::env::var("PERSISTANT_TEST_MYSQL_URL") else {
        assert!(
            std::env::var("PERSISTANT_REQUIRE_MYSQL").as_deref() != Ok("1"),
            "PERSISTANT_REQUIRE_MYSQL=1 but PERSISTANT_TEST_MYSQL_URL is not set"
        );
        eprintln!("skipped: PERSISTANT_TEST_MYSQL_URL is not set");
        return;
    };
    run(&persistant_sqlx::MysqlBackend::connect(&url).await.unwrap()).await;
}
