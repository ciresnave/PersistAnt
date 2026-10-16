// SPDX-License-Identifier: MIT OR Apache-2.0
//! Certify `KvOnRelational` over a real PostgreSQL and print the matrix file on stdout.
//!
//! `PERSISTANT_TEST_DATABASE_URL=postgres://... \
//!    cargo run -q -p persistant-sqlx --example matrix > matrix/sqlx-postgres.tsv`
//!
//! CI regenerates this against its pinned service container and fails if it differs from the
//! checked-in `matrix/sqlx-postgres.tsv`. Rows are keyed by the *server's* version, so a server
//! bump is a visible, reviewed change to the file.

use persistant_core::{Config, Matrix};
use persistant_kv::KvOnRelational;
use persistant_sqlx::PgBackend;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let url = std::env::var("PERSISTANT_TEST_DATABASE_URL")
        .expect("PERSISTANT_TEST_DATABASE_URL must name a PostgreSQL server");
    let db = PgBackend::connect(&url).await.expect("connect");
    let rel_db = db.clone();
    let kv = KvOnRelational::open(db).await.expect("open");
    let env = Config::new()
        .with("os", std::env::consts::OS)
        .with("arch", std::env::consts::ARCH);
    let config = Config::new().with("dialect", "postgres");
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
