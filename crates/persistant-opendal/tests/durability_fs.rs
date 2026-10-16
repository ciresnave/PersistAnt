// SPDX-License-Identifier: MIT OR Apache-2.0
//! `kv.durability.process` on `KvOnBlob` over OpenDAL: kill a writer process after it has
//! acknowledged writes, open the store again, and every acknowledged key must be there. A harness test
//! (`harness = false`): the test binary re-runs itself as the writer child.
//!
//! The **fs** service must hold. The **memory** service cannot (nothing survives the process), so
//! the honest run must be recorded as failing; if it were recorded as holding, the harness would be
//! proving nothing.

use persistant_core::{Config, Reason};
use persistant_kv::durability::{Mode, Run, Target, certify_process, child_loop, maybe_run_child};
use persistant_kv::{KvBackend, KvOnBlob};
use persistant_opendal::OpendalBlob;

fn open(spec: &str) -> KvOnBlob<OpendalBlob> {
    if spec == "memory" {
        return KvOnBlob::new(OpendalBlob::memory().expect("memory"));
    }
    let (root, scratch) = spec.split_once('|').expect("root|scratch");
    KvOnBlob::new(
        OpendalBlob::fs(
            std::path::Path::new(root),
            Some(std::path::Path::new(scratch)),
        )
        .expect("fs"),
    )
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
            let kv = open(spec);
            child_loop(&kv, mode).await.expect("child loop");
        });
    });

    let dir = tempfile::tempdir().expect("temp dir");
    let root = dir.path().join("root");
    let scratch = dir.path().join("scratch");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&scratch).unwrap();
    let spec = format!("{}|{}", root.display(), scratch.display());

    runtime().block_on(async {
        let probe = open(&spec);
        let version = probe.version().to_string();
        let config = Config::new().with("atomic_write_dir", "set");
        let metadata = Config::new();

        let fs_target = Target {
            backend: "kv-on-blob:opendal-fs",
            version: &version,
            config: &config,
            metadata: &metadata,
        };
        let row = certify_process(
            &fs_target,
            Run {
                spec: &spec,
                reopen: || async { open(&spec) },
            },
            Run {
                spec: &spec,
                reopen: || async { open(&spec) },
            },
        )
        .await;
        assert!(
            row.enabled,
            "the fs service lost an acknowledged write: {}",
            row.note
        );
        println!("ok: opendal fs holds (kv.durability.process on)");

        let mem_target = Target {
            backend: "kv-on-blob:opendal-memory",
            ..fs_target
        };
        let row = certify_process(
            &mem_target,
            Run {
                spec: "memory",
                reopen: || async { open("memory") },
            },
            Run {
                spec: "memory",
                reopen: || async { open("memory") },
            },
        )
        .await;
        assert!(!row.enabled, "the memory service was recorded as durable");
        assert_eq!(row.reason, Some(Reason::Wrong), "{}", row.note);
        println!(
            "ok: opendal memory is caught as losing writes ({})",
            row.note
        );
    });
}
