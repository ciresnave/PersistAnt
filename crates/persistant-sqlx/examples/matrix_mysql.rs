// SPDX-License-Identifier: MIT OR Apache-2.0
//! Certify `KvOnRelational` over a real MySQL and print the matrix file on stdout.
//!
//! `PERSISTANT_TEST_MYSQL_URL=mysql://... \
//!    cargo run -q -p persistant-sqlx --features mysql --example matrix_mysql > matrix/sqlx-mysql.tsv`
//!
//! CI regenerates this against its pinned service container and fails if it differs from the
//! checked-in `matrix/sqlx-mysql.tsv`. Rows are keyed by the *server's* version.
//! Against a MariaDB server the same example writes `matrix/sqlx-mariadb.tsv`: the dialect is read from the
//! server.

use persistant_core::{Config, Matrix};
use persistant_kv::KvOnRelational;
use persistant_relational::RelationalBackend;
use persistant_sqlx::MysqlBackend;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let url = std::env::var("PERSISTANT_TEST_MYSQL_URL")
        .expect("PERSISTANT_TEST_MYSQL_URL must name a MySQL server");
    let db = MysqlBackend::connect(&url).await.expect("connect");
    let rel_db = db.clone();
    let kv = KvOnRelational::open(db).await.expect("open");
    let env = Config::new()
        .with("os", std::env::consts::OS)
        .with("arch", std::env::consts::ARCH);
    let config = Config::new().with("dialect", rel_db.dialect().to_string());
    let mut rows: Vec<_> = persistant_kv::conformance::certify(&kv, &config, &env)
        .await
        .into_iter()
        .map(|c| c.row)
        .collect();
    // The relational kind's own leaves, on the same server.
    rows.extend(
        persistant_relational::conformance::certify(&rel_db, &config, &env)
            .await
            .into_iter()
            .map(|c| c.row),
    );
    // The migration ledger, in a scratch database so the ledger is its own.
    rel_db
        .execute("DROP DATABASE IF EXISTS persistant_mig_matrix", &[])
        .await
        .expect("drop scratch database");
    rel_db
        .execute("CREATE DATABASE persistant_mig_matrix", &[])
        .await
        .expect("create scratch database");
    let (head, tail) = url.split_once('?').unwrap_or((url.as_str(), ""));
    let base = head.rsplit_once('/').expect("a database in the url").0;
    let scoped = if tail.is_empty() {
        format!("{base}/persistant_mig_matrix")
    } else {
        format!("{base}/persistant_mig_matrix?{tail}")
    };
    let mig_db = MysqlBackend::connect(&scoped)
        .await
        .expect("connect scoped");
    rows.extend(
        persistant_relational::migrate_conformance::certify(&mig_db, &config, &env)
            .await
            .into_iter()
            .map(|c| c.row),
    );
    drop(mig_db);
    rel_db
        .execute("DROP DATABASE IF EXISTS persistant_mig_matrix", &[])
        .await
        .expect("drop scratch database");
    print!("{}", Matrix::from_rows(rows).to_tsv());
}
