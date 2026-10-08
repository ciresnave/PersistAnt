// SPDX-License-Identifier: MIT OR Apache-2.0
//! Per-call cost of the blocking facade against plain `std::fs`, for a hot path that stores and
//! loads one blob per call (lightbulb's `DiskStore`).
//!
//!     cargo run --release --example blocking_cost
//!
//! Prints the median and p99 of each operation. The numbers depend on the machine and the disk:
//! quote them with the machine and the commit.

use std::time::{Duration, Instant};

use persistant::blocking::Store;
use persistant::{Config, Need, Needs};

fn stats(mut v: Vec<Duration>) -> (Duration, Duration) {
    v.sort();
    (v[v.len() / 2], v[(v.len() * 99 / 100).min(v.len() - 1)])
}

fn show(label: &str, v: Vec<Duration>) {
    let (med, p99) = stats(v);
    println!("  {label:<34} median {med:>10.1?}   p99 {p99:>10.1?}");
}

fn time<F: FnMut()>(n: usize, mut f: F) -> Vec<Duration> {
    (0..n)
        .map(|_| {
            let t = Instant::now();
            f();
            t.elapsed()
        })
        .collect()
}

fn main() {
    let root = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let plain = tempfile::tempdir().unwrap();
    let needs = Needs::new()
        .with(Need::Read)
        .with(Need::Write)
        .with(Need::AtomicReplace);
    let store = Store::open(
        Config::Fs {
            root: root.path().to_path_buf(),
            atomic_write_dir: Some(scratch.path().to_path_buf()),
        },
        needs.clone(),
    )
    .unwrap();
    let mem = Store::open(Config::Memory, needs).unwrap();

    println!("persistant blocking facade vs std::fs, one blob per call");
    for (size, n) in [(4 << 10, 400), (1 << 20, 200), (16 << 20, 30)] {
        let data = vec![0xA5u8; size];
        println!("\n{} KiB, {n} calls each", size >> 10);
        let p = plain.path().join("k");
        show(
            "std::fs::write (no atomicity)",
            time(n, || std::fs::write(&p, &data).unwrap()),
        );
        show(
            "std::fs::read",
            time(n, || drop(std::fs::read(&p).unwrap())),
        );
        show(
            "facade fs replace (atomic)",
            time(n, || store.replace("k", data.clone()).unwrap()),
        );
        show("facade fs read", time(n, || drop(store.read("k").unwrap())));
        show(
            "facade memory replace",
            time(n, || mem.replace("k", data.clone()).unwrap()),
        );
        show(
            "facade memory read",
            time(n, || drop(mem.read("k").unwrap())),
        );
    }
    println!("\nempty-ish round trip (1 byte, memory): pure facade overhead");
    show(
        "facade memory replace 1 B",
        time(5000, || mem.replace("t", vec![1]).unwrap()),
    );
    show(
        "facade memory read 1 B",
        time(5000, || drop(mem.read("t").unwrap())),
    );
}
