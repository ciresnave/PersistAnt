// SPDX-License-Identifier: MIT OR Apache-2.0
//! The catalog readers against real engines: the same tables, read back as the same structure, plus
//! what each engine must NOT report (views, partial and expression indexes, the migration ledger).
//! SQLite always; PostgreSQL and MySQL when their server variables are set.

use persistant_relational::catalog::{Catalog, read_catalog};
use persistant_relational::{Dialect, RelationalBackend};

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
    if matches!(dialect, Dialect::Postgres | Dialect::Sqlite) {
        // Neither makes a conflict target, so neither may appear as a key.
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

    let c = read_catalog(db, Vec::new()).await.unwrap();
    assert_eq!(c.dialect, dialect.to_string());
    let on = format!("on {}", db.backend());

    // Only this test's tables: the database is shared with other tests.
    assert!(c.tables.contains_key(USERS), "{on}: {:?}", c.tables.keys());
    assert!(
        !c.tables.contains_key(VIEW),
        "{on}: a view is not a base table"
    );
    assert!(
        !c.tables.contains_key("_sqlx_migrations"),
        "{on}: the ledger is never in it"
    );

    let users = &c.tables[USERS];
    let names: Vec<_> = users.columns.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(
        names,
        ["id", "email", "note", "age"],
        "{on}: declaration order"
    );
    let col = |n: &str| users.columns.iter().find(|c| c.name == n).unwrap();
    assert!(!col("id").nullable && !col("email").nullable, "{on}");
    assert!(col("note").nullable && col("age").nullable, "{on}");
    assert!(col("age").has_default, "{on}: DEFAULT 7");
    assert!(
        !col("email").has_default && !col("note").has_default,
        "{on}"
    );
    for c in &users.columns {
        assert!(!c.declared_type.is_empty(), "{on}: {} has a type", c.name);
        assert_eq!(c.declared_type, c.declared_type.to_lowercase(), "{on}");
    }
    assert!(
        col("id").declared_type.contains("int"),
        "{on}: {}",
        col("id").declared_type
    );

    assert_eq!(users.primary_key, ["id"], "{on}");
    assert_eq!(
        users.unique_keys,
        [vec!["email".to_string()], vec!["note".to_string()]],
        "{on}: the constraint and the unique index, not the partial or expression ones"
    );
    let pairs = &c.tables[PAIRS];
    assert_eq!(pairs.primary_key, ["a", "b"], "{on}: key order");
    assert!(
        pairs.unique_keys.is_empty(),
        "{on}: the primary key is not also a unique key"
    );
    assert!(pairs.columns.iter().all(|c| !c.nullable), "{on}");

    // The file: deterministic, reads back identical, and a re-read of the database differs in nothing.
    let json = c.to_json();
    assert_eq!(Catalog::from_json(&json).unwrap(), c, "{on}");
    let again = read_catalog(db, Vec::new()).await.unwrap();
    assert!(c.differences(&again).is_empty(), "{on}");

    // And a real change is seen: the control for "differences" being empty above.
    db.execute(
        &format!("ALTER TABLE {PAIRS} ADD COLUMN extra INTEGER"),
        &[],
    )
    .await
    .unwrap();
    let changed = read_catalog(db, Vec::new()).await.unwrap();
    let d = c.differences(&changed).join("\n");
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
