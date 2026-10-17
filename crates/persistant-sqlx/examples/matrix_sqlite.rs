// SPDX-License-Identifier: MIT OR Apache-2.0
//! Certify `KvOnRelational` over a real SQLite file and print the matrix file on stdout.
//!
//! `cargo run -q -p persistant-sqlx --features sqlite --example matrix_sqlite > matrix/sqlx-sqlite.tsv`
//!
//! CI regenerates this and fails if it differs from the checked-in `matrix/sqlx-sqlite.tsv`. Rows
//! are keyed by the SQLite library's version, so a bump of the bundled library is a visible change.

use persistant_core::{Config, Matrix};
use persistant_kv::KvOnRelational;
use persistant_sqlx::SqliteBackend;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let path = std::env::temp_dir().join(format!("persistant-matrix-{}.db", std::process::id()));
    let db = SqliteBackend::open(&path).await.expect("open");
    let rel_db = db.clone();
    let kv = KvOnRelational::open(db).await.expect("open kv");
    let env = Config::new()
        .with("os", std::env::consts::OS)
        .with("arch", std::env::consts::ARCH);
    let config = Config::new().with("dialect", "sqlite");
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
    // The migration ledger: this file is the test own, so the ledger is too.
    rows.extend(
        persistant_relational::migrate_conformance::certify(&rel_db, &config, &env)
            .await
            .into_iter()
            .map(|c| c.row),
    );
    print!("{}", Matrix::from_rows(rows).to_tsv());
    for ext in ["", "-wal", "-shm"] {
        let mut p = path.as_os_str().to_owned();
        p.push(ext);
        let _ = std::fs::remove_file(p);
    }
}
