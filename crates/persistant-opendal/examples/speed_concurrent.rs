// SPDX-License-Identifier: MIT OR Apache-2.0
//! Time the key-value kind (the in-memory reference and `KvOnBlob` on OpenDAL) with several tasks at
//! once and print the speed data file on stdout (`persistant_kv::speed::run_concurrent`).
//!
//! `cargo run -q --release -p persistant-opendal --example speed_concurrent -- --tasks=1,4,16 > bench/opendal-speed-concurrent.tsv`
//!
//! Options: `--tasks=1,4,16` (task counts; default as shown), `--ops=200` (operations per task),
//! `--bytes=256` (value size).
//!
//! **Run it only on a machine nobody else is using**, and say so to whoever else shares it first: the
//! figures depend on every other thing the machine is doing. Each row records the host's CPU load and
//! available RAM at the start and end of its workload (`env` column, `host=`); a row whose load is high
//! at either end is not evidence.

use persistant_core::Config;
use persistant_kv::speed::{Environment, SpeedTable, Workload, host_snapshot, run_concurrent};
use persistant_kv::{KvOnBlob, MemoryKv};
use persistant_opendal::OpendalBlob;
use std::sync::Arc;

fn option<T: std::str::FromStr>(name: &str) -> Option<T> {
    let prefix = format!("--{name}=");
    std::env::args().find_map(|a| a.strip_prefix(&prefix).and_then(|v| v.parse().ok()))
}

fn merged(a: &Config, b: &Config) -> Config {
    let text = [a.to_string(), b.to_string()]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(";");
    Config::parse(&text).expect("config facts parse")
}

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    let tasks: Vec<usize> = std::env::args()
        .find_map(|a| a.strip_prefix("--tasks=").map(str::to_string))
        .map_or_else(
            || vec![1, 4, 16],
            |v| v.split(',').filter_map(|n| n.parse().ok()).collect(),
        );
    let ops: u32 = option("ops").unwrap_or(200);
    let bytes: usize = option("bytes").unwrap_or(256);
    let env = Environment::detect(
        format!(
            "cargo run -q --release -p persistant-opendal --example speed_concurrent -- --tasks={} --ops={ops} --bytes={bytes} > bench/opendal-speed-concurrent.tsv",
            tasks
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(",")
        ),
        None,
    );
    let mut table = SpeedTable::default();

    let root = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let mem = Arc::new(MemoryKv::new());
    let blob_mem = Arc::new(KvOnBlob::new(OpendalBlob::memory().unwrap()));
    let blob_fs = Arc::new(KvOnBlob::new(
        OpendalBlob::fs(root.path(), Some(scratch.path())).unwrap(),
    ));
    let probe = host_snapshot;
    for &n in &tasks {
        let rows = run_concurrent(
            Arc::clone(&mem),
            &Config::new(),
            &env,
            &Workload::ALL,
            ops,
            n,
            bytes,
            &probe,
        )
        .await;
        table.merge(SpeedTable::from_rows(rows));
        for kv in [&blob_mem, &blob_fs] {
            let cfg = merged(kv.blob().config(), &kv.config());
            let rows = run_concurrent(
                Arc::clone(kv),
                &cfg,
                &env,
                &Workload::ALL,
                ops,
                n,
                bytes,
                &probe,
            )
            .await;
            table.merge(SpeedTable::from_rows(rows));
        }
    }
    print!("{}", table.to_tsv());
}
