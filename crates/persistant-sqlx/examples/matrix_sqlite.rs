// SPDX-License-Identifier: MIT OR Apache-2.0
//! Certify `KvOnRelational` over a real SQLite file and print the matrix file on stdout.
//!
//! `cargo run -q -p persistant-sqlx --features sqlite --example matrix_sqlite > matrix/sqlx-sqlite.tsv`
//!
//! CI regenerates this and fails if it differs from the checked-in `matrix/sqlx-sqlite.tsv`. Rows
//! are keyed by the SQLite library's version, so a bump of the bundled library is a visible change.

use persistant_core::{Config, Matrix};
use persistant_kv::KvOnRelational;
use persistant_kv::conformance::Leaf;
use persistant_sqlx::{Bounded, SqliteBackend};
use std::time::Duration;

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
    // The durable profile (`open_durable`: WAL, synchronous=FULL), which the registry offers as
    // `sqlite-full`. Key-value leaves only: that is the layer a registry handle serves.
    let full_path =
        std::env::temp_dir().join(format!("persistant-matrix-full-{}.db", std::process::id()));
    let full = KvOnRelational::open(
        SqliteBackend::open_durable(&full_path)
            .await
            .expect("open durable"),
    )
    .await
    .expect("open kv durable");
    rows.extend(
        persistant_kv::conformance::certify(&full, &config, &env)
            .await
            .into_iter()
            .map(|c| c.row),
    );
    // `kv.bounded_call` with a call bound configured (row config `call_bound=set`), for each
    // profile; the unbounded runs above recorded it as `call_bound=unset`. The tables exist, so
    // opening through the bound only finds them.
    for (name, bounded_path, durable) in
        [("sqlite", &path, false), ("sqlite-full", &full_path, true)]
    {
        let inner = if durable {
            SqliteBackend::open_durable(bounded_path).await
        } else {
            SqliteBackend::open(bounded_path).await
        }
        .unwrap_or_else(|e| panic!("open {name} for the bound: {e}"));
        let bounded = KvOnRelational::open(Bounded::new(inner, Duration::from_millis(2000)))
            .await
            .expect("open bounded");
        rows.extend(
            persistant_kv::conformance::certify_leaves(
                &bounded,
                &config,
                &env,
                &[Leaf::BoundedCall],
            )
            .await
            .into_iter()
            .map(|c| c.row),
        );
    }
    print!("{}", Matrix::from_rows(rows).to_tsv());
    for base in [&path, &full_path] {
        for ext in ["", "-wal", "-shm"] {
            let mut p = base.as_os_str().to_owned();
            p.push(ext);
            let _ = std::fs::remove_file(p);
        }
    }
}
