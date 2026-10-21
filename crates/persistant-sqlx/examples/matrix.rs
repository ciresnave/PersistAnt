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
use persistant_kv::conformance::Leaf;
use persistant_relational::RelationalBackend;
use persistant_sqlx::{Bounded, PgBackend};
use std::time::Duration;

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
    // `kv.bounded_call` for the same server opened with a call bound (row config `call_bound=set`);
    // the unbounded run above recorded it as `call_bound=unset`.
    let bounded = KvOnRelational::open(Bounded::new(
        PgBackend::connect(&url).await.expect("connect bounded"),
        Duration::from_millis(2000),
    ))
    .await
    .expect("open bounded");
    rows.extend(
        persistant_kv::conformance::certify_leaves(&bounded, &config, &env, &[Leaf::BoundedCall])
            .await
            .into_iter()
            .map(|c| c.row),
    );
    // The relational kind's own leaves, on the same server.
    rows.extend(
        persistant_relational::conformance::certify(&rel_db, &config, &env)
            .await
            .into_iter()
            .map(|c| c.row),
    );
    // The migration ledger, in a scratch schema so the ledger is its own.
    rel_db
        .execute("DROP SCHEMA IF EXISTS persistant_mig_matrix CASCADE", &[])
        .await
        .expect("drop scratch schema");
    rel_db
        .execute("CREATE SCHEMA persistant_mig_matrix", &[])
        .await
        .expect("create scratch schema");
    let sep = if url.contains('?') { '&' } else { '?' };
    let scoped = format!("{url}{sep}options=-c%20search_path%3Dpersistant_mig_matrix");
    let mig_db = PgBackend::connect(&scoped).await.expect("connect scoped");
    rows.extend(
        persistant_relational::migrate_conformance::certify(&mig_db, &config, &env)
            .await
            .into_iter()
            .map(|c| c.row),
    );
    drop(mig_db);
    rel_db
        .execute("DROP SCHEMA IF EXISTS persistant_mig_matrix CASCADE", &[])
        .await
        .expect("drop scratch schema");
    print!("{}", Matrix::from_rows(rows).to_tsv());
}
