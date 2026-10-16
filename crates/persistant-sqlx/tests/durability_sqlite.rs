// SPDX-License-Identifier: MIT OR Apache-2.0
//! `kv.durability.process` on SQLite: kill a writer process after it has acknowledged writes, open the
//! database again, and every acknowledged key must be there. A harness test (`harness = false`)
//! because the test binary re-runs itself as the writer child.
//!
//! Two stores through the same harness: a **file** database (must hold) and an **in-memory** one
//! (nothing survives the process, so the honest run must be recorded as failing). If the second were
//! recorded as holding, the harness would be proving nothing.

use persistant_core::{Config, Reason};
use persistant_kv::KvOnRelational;
use persistant_kv::durability::{Mode, Run, Target, certify_process, child_loop, maybe_run_child};
use persistant_relational::RelationalBackend;
use persistant_sqlx::SqliteBackend;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

async fn open(spec: &str) -> KvOnRelational<SqliteBackend> {
    let db = if spec == "memory" {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(SqliteConnectOptions::new().in_memory(true))
            .await
            .expect("memory pool");
        SqliteBackend::from_pool(pool).await.expect("from_pool")
    } else {
        SqliteBackend::open(spec).await.expect("open file")
    };
    KvOnRelational::open(db).await.expect("open kv")
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
}

fn main() {
    maybe_run_child(|spec, mode: Mode| {
        runtime().block_on(async {
            let kv = open(spec).await;
            child_loop(&kv, mode).await.expect("child loop");
        });
    });

    let dir = std::env::temp_dir().join(format!("persistant-durability-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let honest_file = dir.join("honest.db");
    let lying_file = dir.join("lying.db");
    let (honest_file, lying_file) = (
        honest_file.to_string_lossy().into_owned(),
        lying_file.to_string_lossy().into_owned(),
    );

    let rt = runtime();
    rt.block_on(async {
        let probe = open(&honest_file).await;
        let version = RelationalBackend::version(probe.backend_handle()).to_string();
        drop(probe);
        let config = Config::new().with("dialect", "sqlite");
        let metadata = Config::new();
        let target = Target {
            backend: "kv-on-relational:sqlite",
            version: &version,
            config: &config,
            metadata: &metadata,
        };

        // A file database must hold what it acknowledged.
        let row = certify_process(
            &target,
            Run {
                spec: &honest_file,
                reopen: || open(&honest_file),
            },
            Run {
                spec: &lying_file,
                reopen: || open(&lying_file),
            },
        )
        .await;
        assert!(
            row.enabled,
            "SQLite file lost an acknowledged write: {}",
            row.note
        );
        println!("ok: sqlite file holds (kv.durability.process on)");

        // An in-memory database cannot: the honest run must come out as `Wrong`.
        let row = certify_process(
            &target,
            Run {
                spec: "memory",
                reopen: || open("memory"),
            },
            Run {
                spec: "memory",
                reopen: || open("memory"),
            },
        )
        .await;
        assert!(
            !row.enabled,
            "an in-memory database was recorded as durable"
        );
        assert_eq!(row.reason, Some(Reason::Wrong), "{}", row.note);
        println!(
            "ok: in-memory database is caught as losing writes ({})",
            row.note
        );
    });
    let _ = std::fs::remove_dir_all(&dir);
}
