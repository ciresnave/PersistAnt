// SPDX-License-Identifier: MIT OR Apache-2.0
//! `sqlx_query!`, `sqlx_query_as!` and `sqlx_query_scalar!` end to end (`docs/DESIGN-SQLX-BRIDGE.md`):
//! the result types come from the committed `.sqlx` files beside this crate (hand-made for the three
//! statements below: the sqlx check itself needs a server and is not in this build), the statement is
//! translated by UniSQL and run by each engine's own text. SQLite always; PostgreSQL when its server
//! variable is set. `sqlx = none`: this crate has no sqlx macros (a workspace build with every feature
//! turns the check on for every call).

use persistant_macros::{FromRow, sqlx_query, sqlx_query_as, sqlx_query_scalar};
use persistant_relational::{InTx, Isolation, RelationalBackend, RelationalTx};

#[derive(Debug, PartialEq, FromRow)]
#[persistant(crate = ::persistant_relational)]
struct Item {
    id: i64,
    name: String,
    note: Option<String>,
}

async fn run<B: RelationalBackend>(db: &B) {
    db.execute("DROP TABLE IF EXISTS persistant_sqlxq", &[])
        .await
        .unwrap();
    db.execute(
        "CREATE TABLE persistant_sqlxq (id BIGINT PRIMARY KEY, name VARCHAR(40) NOT NULL, note VARCHAR(40))",
        &[],
    )
    .await
    .unwrap();

    for (id, name, note) in [
        (1_i64, "one", None),
        (2, "two", Some("second")),
        (5, "five", None),
    ] {
        let n = sqlx_query!(
            crate = ::persistant_relational,
            sqlx = none,
            targets = [postgres, sqlite],
            "INSERT INTO persistant_sqlxq (id, name, note) VALUES ($1, $2, $3)",
            id,
            name,
            note
        )
        .execute(db)
        .await
        .unwrap();
        assert_eq!(n, 1, "on {}", db.backend());
    }

    // The anonymous record: fields by name, `note` an Option because the column can be NULL.
    let rows = sqlx_query!(
        crate = ::persistant_relational,
        sqlx = none,
        targets = [postgres, sqlite],
        allow = [text_collation],
        "SELECT id, name, note FROM persistant_sqlxq WHERE id >= $1 ORDER BY id",
        2_i64
    )
    .fetch_all(db)
    .await
    .unwrap();
    let got: Vec<(i64, &str, Option<&str>)> = rows
        .iter()
        .map(|r| (r.id, r.name.as_str(), r.note.as_deref()))
        .collect();
    assert_eq!(
        got,
        [(2, "two", Some("second")), (5, "five", None)],
        "on {}",
        db.backend()
    );

    // The caller's own struct, names and nullability checked against the description at build time.
    let one: Item = sqlx_query_as!(
        crate = ::persistant_relational,
        sqlx = none,
        targets = [postgres, sqlite],
        allow = [text_collation],
        Item,
        "SELECT id, name, note FROM persistant_sqlxq WHERE id >= $1 ORDER BY id",
        5_i64
    )
    .fetch_one(db)
    .await
    .unwrap();
    assert_eq!(
        one,
        Item {
            id: 5,
            name: "five".into(),
            note: None
        }
    );
    let none = sqlx_query_as!(
        crate = ::persistant_relational,
        sqlx = none,
        targets = [postgres, sqlite],
        allow = [text_collation],
        Item,
        "SELECT id, name, note FROM persistant_sqlxq WHERE id >= $1 ORDER BY id",
        6_i64
    )
    .fetch_optional(db)
    .await
    .unwrap();
    assert_eq!(none, None);

    let count: i64 = sqlx_query_scalar!(
        crate = ::persistant_relational,
        sqlx = none,
        targets = [postgres, sqlite],
        allow = [text_collation],
        r#"SELECT count(*) AS "n!" FROM persistant_sqlxq"#
    )
    .fetch_one(db)
    .await
    .unwrap();
    assert_eq!(count, 3);

    // Inside a transaction, rolled back.
    let dialect = db.dialect();
    let mut tx = db.begin(Isolation::ReadCommitted).await.unwrap();
    sqlx_query!(
        crate = ::persistant_relational,
        sqlx = none,
        targets = [postgres, sqlite],
        allow = [text_collation],
        "INSERT INTO persistant_sqlxq (id, name, note) VALUES ($1, $2, $3)",
        9_i64,
        "nine",
        None::<String>
    )
    .execute(InTx::new(&mut tx, dialect.clone()))
    .await
    .unwrap();
    let inside: i64 = sqlx_query_scalar!(
        crate = ::persistant_relational,
        sqlx = none,
        targets = [postgres, sqlite],
        allow = [text_collation],
        r#"SELECT count(*) AS "n!" FROM persistant_sqlxq"#
    )
    .fetch_one(InTx::new(&mut tx, dialect))
    .await
    .unwrap();
    assert_eq!(inside, 4);
    tx.rollback().await.unwrap();

    db.execute("DROP TABLE persistant_sqlxq", &[])
        .await
        .unwrap();
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn on_sqlite() {
    let path = std::env::temp_dir().join(format!("persistant-sqlxq-{}.db", std::process::id()));
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
