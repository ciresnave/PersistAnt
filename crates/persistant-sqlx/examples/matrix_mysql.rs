// SPDX-License-Identifier: MIT OR Apache-2.0
//! Certify `KvOnRelational` over a real MySQL and print the matrix file on stdout.
//!
//! `PERSISTANT_TEST_MYSQL_URL=mysql://... \
//!    cargo run -q -p persistant-sqlx --features mysql --example matrix_mysql > matrix/sqlx-mysql.tsv`
//!
//! CI regenerates this against its pinned service container and fails if it differs from the
//! checked-in `matrix/sqlx-mysql.tsv`. Rows are keyed by the *server's* version.

use persistant_core::{Config, Matrix};
use persistant_kv::KvOnRelational;
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
    let config = Config::new().with("dialect", "mysql");
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
    print!("{}", Matrix::from_rows(rows).to_tsv());
}
