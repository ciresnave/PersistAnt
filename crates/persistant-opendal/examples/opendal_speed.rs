// SPDX-License-Identifier: MIT OR Apache-2.0
//! Time the key-value kind (the in-memory reference and `KvOnBlob` on OpenDAL) and the blob kind
//! (OpenDAL memory and fs) and print the
//! speed data file on stdout (stage 3 of backend benchmarking, `persistant_kv::speed`).
//!
//! `cargo run -q --release -p persistant-opendal --example opendal_speed > bench/opendal-speed.tsv`
//!
//! A backend that fails the contract for a workload is listed with its reason and not timed. The
//! file carries the machine, OS, date and this command; a figure is only meaningful there.

use persistant_blob::speed::Workload as BlobWorkload;
use persistant_core::Config;
use persistant_kv::speed::{Environment, SpeedTable, Workload, run};
use persistant_kv::{KvOnBlob, MemoryKv};
use persistant_opendal::OpendalBlob;

const ITERATIONS: u32 = 200;

fn merged(a: &Config, b: &Config) -> Config {
    let text = [a.to_string(), b.to_string()]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(";");
    Config::parse(&text).expect("config facts parse")
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let env = Environment::detect(
        "cargo run -q --release -p persistant-opendal --example opendal_speed > bench/opendal-speed.tsv",
        None,
    );
    let mut table = SpeedTable::default();

    let mem = MemoryKv::new();
    table.merge(SpeedTable::from_rows(
        run(&mem, &Config::new(), &env, &Workload::ALL, ITERATIONS).await,
    ));

    let root = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let kvs = [
        KvOnBlob::new(OpendalBlob::memory().unwrap()),
        KvOnBlob::new(OpendalBlob::fs(root.path(), Some(scratch.path())).unwrap()),
    ];
    for kv in &kvs {
        let cfg = merged(kv.blob().config(), &kv.config());
        let rows = run(kv, &cfg, &env, &Workload::ALL, ITERATIONS).await;
        table.merge(SpeedTable::from_rows(rows));
    }

    // The blob kind itself, on the same services (a fresh directory for the fs one).
    let blob_root = tempfile::tempdir().unwrap();
    let blob_scratch = tempfile::tempdir().unwrap();
    let blobs = [
        OpendalBlob::memory().unwrap(),
        OpendalBlob::fs(blob_root.path(), Some(blob_scratch.path())).unwrap(),
    ];
    for b in &blobs {
        let rows =
            persistant_blob::speed::run(b, b.config(), &env, &BlobWorkload::ALL, ITERATIONS).await;
        table.merge(SpeedTable::from_rows(rows));
    }
    print!("{}", table.to_tsv());
}
