// SPDX-License-Identifier: MIT OR Apache-2.0
//! `kv.durability.process` on postgres: kill a *client* process after it has acknowledged writes, open
//! a new connection, and every acknowledged key must be there. A harness test (`harness = false`).
//!
//! The server outliving its client is the `Process` class for a database; what a server does when
//! its own machine loses power is the `Machine` class, which this does not test and which stays
//! declared. Needs `PERSISTANT_TEST_DATABASE_URL`; without it the test prints a note and passes, unless
//! `PERSISTANT_REQUIRE_DATABASE=1` (set by the CI job that provides the server).

use persistant_core::Config;
use persistant_kv::KvOnRelational;
use persistant_kv::durability::{
    Mode, Run, Target, certify_process, child_loop, cleanup, maybe_run_child,
};
use persistant_relational::RelationalBackend;
use persistant_sqlx::PgBackend;

async fn open(spec: &str) -> KvOnRelational<PgBackend> {
    let db = PgBackend::connect(spec).await.expect("connect");
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

    let Ok(url) = std::env::var("PERSISTANT_TEST_DATABASE_URL") else {
        assert!(
            std::env::var("PERSISTANT_REQUIRE_DATABASE").as_deref() != Ok("1"),
            "PERSISTANT_REQUIRE_DATABASE=1 but PERSISTANT_TEST_DATABASE_URL is not set"
        );
        eprintln!("skipped: PERSISTANT_TEST_DATABASE_URL is not set");
        return;
    };
    runtime().block_on(async {
        let probe = open(&url).await;
        let version = RelationalBackend::version(probe.backend_handle()).to_string();
        let name = format!(
            "kv-on-relational:{}",
            RelationalBackend::backend(probe.backend_handle())
        );
        drop(probe);
        let config = Config::new().with("dialect", "postgres");
        let metadata = Config::new();
        let target = Target {
            backend: &name,
            version: &version,
            config: &config,
            metadata: &metadata,
        };
        let row = certify_process(
            &target,
            Run {
                spec: &url,
                reopen: || open(&url),
            },
            Run {
                spec: &url,
                reopen: || open(&url),
            },
        )
        .await;
        assert!(
            row.enabled,
            "postgres lost an acknowledged write: {}",
            row.note
        );
        cleanup(&open(&url).await).await.expect("cleanup");
        println!("ok: postgres holds what a killed client acknowledged (kv.durability.process on)");
    });
}
