// SPDX-License-Identifier: MIT OR Apache-2.0
//! Role-based access control written on `RelationalBackend`: store a role, assign it to a user, list
//! a role's users, take it away. This is the worked example `docs/DESIGN-AUTH-STORAGE.md` section 4
//! points at (PM [TASK] 2026-11-09, ROADMAP row 26): PersistAnt has **no role API**; a program owns
//! its schema and writes plain SQL, and the same function below runs on every SQL backend.
//!
//! What it shows, and the tests check:
//! * a role is a row, a user-to-role assignment is a row in a join table with a composite primary key;
//! * assigning twice is refused as a unique violation (`RelError::UniqueViolation`), so a caller can treat
//!   "already assigned" as success without a read-then-write race;
//! * removing a role removes its assignments in the same transaction, and a rolled-back transaction
//!   removes nothing;
//! * the only dialect difference a program sees is the placeholder (`$1`, `?1`, `?`) and the text type.
//!
//! SQLite needs no server and always runs. PostgreSQL and MySQL/MariaDB run when
//! `PERSISTANT_TEST_DATABASE_URL` / `PERSISTANT_TEST_MYSQL_URL` is set (and fail instead of skipping
//! when `PERSISTANT_REQUIRE_DATABASE=1` / `PERSISTANT_REQUIRE_MYSQL=1`, as CI sets them).

use persistant_relational::{Dialect, Isolation, RelError, RelationalBackend, RelationalTx, Value};

/// The placeholder for parameter `n` (1-based) in `d`.
fn p(d: &Dialect, n: usize) -> String {
    match d {
        Dialect::Postgres => format!("${n}"),
        Dialect::Sqlite => format!("?{n}"),
        _ => "?".to_string(),
    }
}

fn text(s: &str) -> Value {
    Value::Text(s.to_string())
}

/// The table names are unique per run so tests on one shared server do not meet.
struct Rbac {
    roles: String,
    assignments: String,
}

impl Rbac {
    fn new(tag: &str) -> Self {
        let n = format!(
            "{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
                % 1_000_000_000
        );
        Rbac {
            roles: format!("rbac_roles_{tag}_{n}"),
            assignments: format!("rbac_user_roles_{tag}_{n}"),
        }
    }

    async fn create<B: RelationalBackend>(&self, db: &B) -> Result<(), RelError> {
        // VARCHAR(255) is accepted by all four dialects; MySQL cannot key a bare TEXT column.
        db.execute(
            &format!(
                "CREATE TABLE {} (name VARCHAR(255) PRIMARY KEY, description VARCHAR(255) NOT NULL)",
                self.roles
            ),
            &[],
        )
        .await?;
        db.execute(
            &format!(
                "CREATE TABLE {} (user_id VARCHAR(255) NOT NULL, role VARCHAR(255) NOT NULL, \
                 PRIMARY KEY (user_id, role))",
                self.assignments
            ),
            &[],
        )
        .await?;
        Ok(())
    }

    async fn drop_tables<B: RelationalBackend>(&self, db: &B) {
        let _ = db
            .execute(&format!("DROP TABLE {}", self.assignments), &[])
            .await;
        let _ = db.execute(&format!("DROP TABLE {}", self.roles), &[]).await;
    }

    async fn store_role<B: RelationalBackend>(
        &self,
        db: &B,
        name: &str,
        description: &str,
    ) -> Result<(), RelError> {
        let d = db.dialect();
        db.execute(
            &format!(
                "INSERT INTO {} (name, description) VALUES ({}, {})",
                self.roles,
                p(&d, 1),
                p(&d, 2)
            ),
            &[text(name), text(description)],
        )
        .await
        .map(|_| ())
    }

    /// `Ok(true)` if newly assigned, `Ok(false)` if the user already had the role.
    async fn assign<B: RelationalBackend>(
        &self,
        db: &B,
        user: &str,
        role: &str,
    ) -> Result<bool, RelError> {
        let d = db.dialect();
        match db
            .execute(
                &format!(
                    "INSERT INTO {} (user_id, role) VALUES ({}, {})",
                    self.assignments,
                    p(&d, 1),
                    p(&d, 2)
                ),
                &[text(user), text(role)],
            )
            .await
        {
            Ok(_) => Ok(true),
            Err(RelError::UniqueViolation { .. }) => Ok(false),
            Err(e) => Err(e),
        }
    }

    async fn role_users<B: RelationalBackend>(
        &self,
        db: &B,
        role: &str,
    ) -> Result<Vec<String>, RelError> {
        let d = db.dialect();
        let rows = db
            .query(
                &format!(
                    "SELECT user_id FROM {} WHERE role = {} ORDER BY user_id",
                    self.assignments,
                    p(&d, 1)
                ),
                &[text(role)],
            )
            .await?;
        Ok(rows
            .iter()
            .map(|r| r.text(0).expect("user_id is text").to_string())
            .collect())
    }

    async fn user_roles<B: RelationalBackend>(
        &self,
        db: &B,
        user: &str,
    ) -> Result<Vec<String>, RelError> {
        let d = db.dialect();
        let rows = db
            .query(
                &format!(
                    "SELECT role FROM {} WHERE user_id = {} ORDER BY role",
                    self.assignments,
                    p(&d, 1)
                ),
                &[text(user)],
            )
            .await?;
        Ok(rows
            .iter()
            .map(|r| r.text(0).expect("role is text").to_string())
            .collect())
    }

    /// Remove a role and every assignment of it in one transaction; `commit` says whether to keep it.
    async fn delete_role<B: RelationalBackend>(
        &self,
        db: &B,
        role: &str,
        commit: bool,
    ) -> Result<u64, RelError> {
        let d = db.dialect();
        let mut tx = db.begin(Isolation::ReadCommitted).await?;
        tx.execute(
            &format!("DELETE FROM {} WHERE role = {}", self.assignments, p(&d, 1)),
            &[text(role)],
        )
        .await?;
        let removed = tx
            .execute(
                &format!("DELETE FROM {} WHERE name = {}", self.roles, p(&d, 1)),
                &[text(role)],
            )
            .await?;
        if commit {
            tx.commit().await?;
        } else {
            tx.rollback().await?;
        }
        Ok(removed)
    }
}

async fn exercise<B: RelationalBackend>(db: &B, tag: &str) {
    let rbac = Rbac::new(tag);
    rbac.create(db).await.expect("create tables");

    rbac.store_role(db, "admin", "everything")
        .await
        .expect("store admin");
    rbac.store_role(db, "reader", "read only")
        .await
        .expect("store reader");
    // Storing the same role twice is a unique violation the caller can see.
    assert!(matches!(
        rbac.store_role(db, "admin", "again").await,
        Err(RelError::UniqueViolation { .. })
    ));

    assert!(rbac.assign(db, "alice", "admin").await.unwrap());
    assert!(rbac.assign(db, "bob", "admin").await.unwrap());
    assert!(rbac.assign(db, "alice", "reader").await.unwrap());
    // Already assigned: false, not an error and not a second row.
    assert!(!rbac.assign(db, "alice", "admin").await.unwrap());

    assert_eq!(
        rbac.role_users(db, "admin").await.unwrap(),
        ["alice", "bob"]
    );
    assert_eq!(rbac.role_users(db, "reader").await.unwrap(), ["alice"]);
    assert_eq!(
        rbac.user_roles(db, "alice").await.unwrap(),
        ["admin", "reader"]
    );
    assert!(rbac.role_users(db, "nobody").await.unwrap().is_empty());

    // A rolled-back removal removes nothing.
    assert_eq!(rbac.delete_role(db, "admin", false).await.unwrap(), 1);
    assert_eq!(
        rbac.role_users(db, "admin").await.unwrap(),
        ["alice", "bob"]
    );

    // A committed removal takes the assignments with it; the other role is untouched.
    assert_eq!(rbac.delete_role(db, "admin", true).await.unwrap(), 1);
    assert!(rbac.role_users(db, "admin").await.unwrap().is_empty());
    assert_eq!(rbac.user_roles(db, "alice").await.unwrap(), ["reader"]);

    rbac.drop_tables(db).await;
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn rbac_on_sqlite() {
    let path = std::env::temp_dir().join(format!("persistant-rbac-{}.db", std::process::id()));
    let db = persistant_sqlx::SqliteBackend::open(&path)
        .await
        .expect("open");
    exercise(&db, "sqlite").await;
    drop(db);
    for ext in ["", "-wal", "-shm"] {
        let mut f = path.as_os_str().to_owned();
        f.push(ext);
        let _ = std::fs::remove_file(f);
    }
}

#[tokio::test]
async fn rbac_on_postgres() {
    match std::env::var("PERSISTANT_TEST_DATABASE_URL") {
        Ok(url) => {
            let db = persistant_sqlx::PgBackend::connect(&url)
                .await
                .expect("connect");
            exercise(&db, "pg").await;
        }
        Err(_) => {
            assert!(
                std::env::var("PERSISTANT_REQUIRE_DATABASE").as_deref() != Ok("1"),
                "PERSISTANT_REQUIRE_DATABASE=1 but PERSISTANT_TEST_DATABASE_URL is not set"
            );
            eprintln!("skipped: PERSISTANT_TEST_DATABASE_URL is not set");
        }
    }
}

#[cfg(feature = "mysql")]
#[tokio::test]
async fn rbac_on_mysql() {
    match std::env::var("PERSISTANT_TEST_MYSQL_URL") {
        Ok(url) => {
            let db = persistant_sqlx::MysqlBackend::connect(&url)
                .await
                .expect("connect");
            exercise(&db, "mysql").await;
        }
        Err(_) => {
            assert!(
                std::env::var("PERSISTANT_REQUIRE_MYSQL").as_deref() != Ok("1"),
                "PERSISTANT_REQUIRE_MYSQL=1 but PERSISTANT_TEST_MYSQL_URL is not set"
            );
            eprintln!("skipped: PERSISTANT_TEST_MYSQL_URL is not set");
        }
    }
}
