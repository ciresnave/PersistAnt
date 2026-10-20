// SPDX-License-Identifier: MIT OR Apache-2.0
//! The catalog readers against real engines: the same tables, read back as the same structure, plus
//! what each engine must NOT report (views, the migration ledger) and what it must flag (a partial or
//! expression unique index makes the table's key list unreliable). SQLite always; PostgreSQL and
//! MySQL when their server variables are set.

use persistant_relational::catalog::{self, ColumnClass, differences, read_catalog};
use persistant_relational::{Dialect, RelationalBackend};
use std::collections::BTreeMap;

const USERS: &str = "persistant_cat_users";
const PAIRS: &str = "persistant_cat_pairs";
const VIEW: &str = "persistant_cat_view";

async fn run<B: RelationalBackend>(db: &B) {
    let dialect = db.dialect();
    for sql in [
        format!("DROP VIEW IF EXISTS {VIEW}"),
        format!("DROP TABLE IF EXISTS {USERS}"),
        format!("DROP TABLE IF EXISTS {PAIRS}"),
    ] {
        db.execute(&sql, &[]).await.unwrap();
    }
    let mut ddl = vec![
        format!(
            "CREATE TABLE {USERS} (id BIGINT NOT NULL PRIMARY KEY, email VARCHAR(80) NOT NULL, \
             note VARCHAR(80), age INTEGER DEFAULT 7, UNIQUE (email))"
        ),
        format!(
            "CREATE TABLE {PAIRS} (a INTEGER NOT NULL, b INTEGER NOT NULL, PRIMARY KEY (a, b))"
        ),
        format!("CREATE UNIQUE INDEX {USERS}_note ON {USERS} (note)"),
        format!("CREATE VIEW {VIEW} AS SELECT id FROM {USERS}"),
    ];
    let has_odd_indexes = matches!(dialect, Dialect::Postgres | Dialect::Sqlite);
    if has_odd_indexes {
        // Neither makes a conflict target: the table's key list must be flagged unreliable, not
        // silently shortened.
        ddl.push(format!(
            "CREATE UNIQUE INDEX {USERS}_partial ON {USERS} (age) WHERE age > 100"
        ));
        ddl.push(format!(
            "CREATE UNIQUE INDEX {USERS}_expr ON {USERS} (lower(email))"
        ));
    }
    for sql in &ddl {
        db.execute(sql, &[]).await.unwrap();
    }

    let c = read_catalog(db, BTreeMap::new()).await.unwrap();
    let on = format!("on {}", db.backend());
    assert_eq!(c.meta["dialect"], dialect.to_string(), "{on}");

    // Only this test's tables matter: the database is shared with other tests.
    let users = c
        .schema
        .tables()
        .find(|t| t.name == [USERS])
        .unwrap_or_else(|| panic!("{on}: no {USERS}"));
    assert!(
        !c.schema.tables().any(|t| t.name == [VIEW]),
        "{on}: a view is not a base table"
    );
    assert!(
        !c.schema.tables().any(|t| t.name == ["_sqlx_migrations"]),
        "{on}: the ledger is never in it"
    );

    let names: Vec<_> = users.columns.keys().map(String::as_str).collect();
    assert_eq!(names, ["age", "email", "id", "note"], "{on}: name order");
    assert!(
        users.columns_complete,
        "{on}: the column list was read in full"
    );
    let col = |n: &str| &users.columns[n];
    assert_eq!(col("id").nullable, Some(false), "{on}");
    assert_eq!(col("email").nullable, Some(false), "{on}");
    assert_eq!(col("note").nullable, Some(true), "{on}");
    assert_eq!(col("age").nullable, Some(true), "{on}");
    assert_eq!(col("age").has_default, Some(true), "{on}: DEFAULT 7");
    assert_eq!(col("email").has_default, Some(false), "{on}");
    assert_eq!(col("note").has_default, Some(false), "{on}");
    // Classified by the engine's own naming of the type; none is left Unknown here.
    assert_eq!(col("id").class, ColumnClass::Numeric, "{on}");
    assert_eq!(col("age").class, ColumnClass::Numeric, "{on}");
    assert_eq!(col("email").class, ColumnClass::Text, "{on}");
    assert_eq!(col("note").class, ColumnClass::Text, "{on}");
    for (n, c) in users.columns {
        let t = c.declared_type.as_deref().unwrap_or_default();
        assert!(!t.is_empty(), "{on}: {n} has a declared type");
        assert_eq!(t, t.to_lowercase(), "{on}");
    }
    assert!(
        col("id").declared_type.as_deref().unwrap().contains("int"),
        "{on}: {:?}",
        col("id").declared_type
    );

    // The primary key first, then the other unique keys in order.
    assert_eq!(
        users.unique_keys,
        [
            vec!["id".to_string()],
            vec!["email".to_string()],
            vec!["note".to_string()]
        ],
        "{on}"
    );
    assert_eq!(
        users.keys_unreliable, has_odd_indexes,
        "{on}: a partial or expression unique index flags the list, and nothing else does"
    );
    let pairs = c.schema.tables().find(|t| t.name == [PAIRS]).unwrap();
    assert_eq!(
        pairs.unique_keys,
        [vec!["a".to_string(), "b".to_string()]],
        "{on}: key order"
    );
    assert!(!pairs.keys_unreliable, "{on}");
    assert!(
        pairs.columns.values().all(|c| c.nullable == Some(false)),
        "{on}"
    );

    // The file: deterministic, reads back identical, and a re-read of the database differs in nothing.
    let json = c.to_json();
    assert_eq!(catalog::parse(&json).unwrap(), c, "{on}");
    let again = read_catalog(db, BTreeMap::new()).await.unwrap();
    assert!(differences(&c, &again).is_empty(), "{on}");

    // And a real change is seen: the control for "differences" being empty above.
    db.execute(
        &format!("ALTER TABLE {PAIRS} ADD COLUMN extra INTEGER"),
        &[],
    )
    .await
    .unwrap();
    let changed = read_catalog(db, BTreeMap::new()).await.unwrap();
    let d = differences(&c, &changed).join("\n");
    assert!(
        d.contains(&format!("column {PAIRS}.extra: not in the snapshot")),
        "{on}: {d}"
    );

    for sql in [
        format!("DROP VIEW {VIEW}"),
        format!("DROP TABLE {USERS}"),
        format!("DROP TABLE {PAIRS}"),
    ] {
        db.execute(&sql, &[]).await.unwrap();
    }
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn on_sqlite() {
    let path = std::env::temp_dir().join(format!("persistant-catalog-{}.db", std::process::id()));
    let db = persistant_sqlx::SqliteBackend::open(&path).await.unwrap();
    run(&db).await;
    drop(db);
    for ext in ["", "-wal", "-shm"] {
        let mut p = path.as_os_str().to_owned();
        p.push(ext);
        let _ = std::fs::remove_file(p);
    }
}

/// One table name per test: one at a time per server.
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
