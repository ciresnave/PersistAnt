// SPDX-License-Identifier: MIT OR Apache-2.0
//! Time `KvOnRelational` over SQLite (always), PostgreSQL (`PERSISTANT_TEST_DATABASE_URL`) and MySQL
//! (feature `mysql` and `PERSISTANT_TEST_MYSQL_URL`), and print the speed data file on stdout
//! (stage 3 of backend benchmarking, `persistant_kv::speed`).
//!
//! `cargo run -q --release -p persistant-sqlx --features sqlite,mysql --example sqlx_speed > bench/sqlx-speed.tsv`
//!
//! Set `PERSISTANT_SPEED_POSTGRES_DIGEST` / `PERSISTANT_SPEED_MYSQL_DIGEST` to the server image digest
//! when a server runs in a container; it is recorded with that backend's rows. A network database includes its round trip: that is the cost a consumer
//! pays, and it is part of why a figure holds only in the environment it was taken in.

use persistant_core::Config;
use persistant_kv::KvOnRelational;
use persistant_kv::speed::{Environment, SpeedTable, Workload, run};
use persistant_sqlx::{PgBackend, SqliteBackend};

const ITERATIONS: u32 = 100;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let env = Environment::detect(
        "cargo run -q --release -p persistant-sqlx --features sqlite,mysql --example sqlx_speed > bench/sqlx-speed.tsv",
        None,
    );
    // A server in a container has an image digest; SQLite is a library and has none.
    let with_digest = |var: &str| Environment {
        digest: std::env::var(var).ok(),
        ..env.clone()
    };
    let mut table = SpeedTable::default();

    let path = std::env::temp_dir().join(format!("persistant-speed-{}.db", std::process::id()));
    let sqlite = KvOnRelational::open(SqliteBackend::open(&path).await.expect("sqlite"))
        .await
        .expect("open sqlite");
    let cfg = Config::new().with("dialect", "sqlite");
    table.merge(SpeedTable::from_rows(
        run(&sqlite, &cfg, &env, &Workload::ALL, ITERATIONS).await,
    ));
    drop(sqlite);
    for ext in ["", "-wal", "-shm"] {
        let mut p = path.as_os_str().to_owned();
        p.push(ext);
        let _ = std::fs::remove_file(p);
    }

    if let Ok(url) = std::env::var("PERSISTANT_TEST_DATABASE_URL") {
        let pg = KvOnRelational::open(PgBackend::connect(&url).await.expect("postgres"))
            .await
            .expect("open postgres");
        let cfg = Config::new().with("dialect", "postgres");
        table.merge(SpeedTable::from_rows(
            run(
                &pg,
                &cfg,
                &with_digest("PERSISTANT_SPEED_POSTGRES_DIGEST"),
                &Workload::ALL,
                ITERATIONS,
            )
            .await,
        ));
    }

    #[cfg(feature = "mysql")]
    if let Ok(url) = std::env::var("PERSISTANT_TEST_MYSQL_URL") {
        let my = KvOnRelational::open(
            persistant_sqlx::MysqlBackend::connect(&url)
                .await
                .expect("mysql"),
        )
        .await
        .expect("open mysql");
        let cfg = Config::new().with("dialect", "mysql");
        table.merge(SpeedTable::from_rows(
            run(
                &my,
                &cfg,
                &with_digest("PERSISTANT_SPEED_MYSQL_DIGEST"),
                &Workload::ALL,
                ITERATIONS,
            )
            .await,
        ));
    }

    print!("{}", table.to_tsv());
}
