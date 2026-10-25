// SPDX-License-Identifier: MIT OR Apache-2.0
//! Read one or more speed data files and print, per workload and per environment, the backends that
//! have a number (fastest median first) and the ones that were excluded and why.
//!
//! `cargo run -q -p persistant-kv --example speed_report -- bench/opendal-speed.tsv bench/sqlx-speed.tsv`
//!
//! `--across=speed.kv.get,speed.kv.put` prints, instead, each backend's place in every named workload
//! side by side, with the backends another one beats everywhere. There is no combined score.
//!
//! The output is derived from the files and says where it holds: an ordering is printed under the
//! machine it was measured on, never as a property of the backend.

use persistant_kv::speed::{SpeedTable, compare, compare_across};

fn main() {
    let mut table = SpeedTable::default();
    let (flags, paths): (Vec<String>, Vec<String>) = std::env::args()
        .skip(1)
        .partition(|a| a.starts_with("--across="));
    assert!(
        !paths.is_empty(),
        "usage: speed_report FILE.tsv [FILE.tsv ...]"
    );
    for p in &paths {
        let text = std::fs::read_to_string(p).unwrap_or_else(|e| panic!("{p}: {e}"));
        table.merge(SpeedTable::parse(&text).unwrap_or_else(|e| panic!("{p}: {e}")));
    }
    if let Some(list) = flags.last().and_then(|f| f.strip_prefix("--across=")) {
        let workloads: Vec<&str> = list.split(',').collect();
        for p in compare_across(&table, &workloads) {
            println!("[{}]", p.environment);
            for x in &p.positions {
                let cells: Vec<String> = p
                    .workloads
                    .iter()
                    .zip(&x.places)
                    .map(|(w, place)| match place {
                        Some((n, p50)) => format!("{w} #{n} ({:.1} us)", *p50 as f64 / 1000.0),
                        None => format!("{w} -"),
                    })
                    .collect();
                let beaten = if x.dominated_by.is_empty() {
                    String::new()
                } else {
                    format!("  [beaten everywhere by {}]", x.dominated_by.join(", "))
                };
                println!(
                    "  {} {} {}: {}{beaten}",
                    x.backend,
                    x.version,
                    x.config,
                    cells.join("; ")
                );
            }
        }
        return;
    }
    for c in compare(&table) {
        println!("{}  [{}]", c.workload, c.environment);
        for (i, r) in c.ranked.iter().enumerate() {
            println!(
                "  {}. {} {} {}  p50 {:.1} us  p95 {:.1} us  {} ops/s  ({}; re-run: {})",
                i + 1,
                r.backend,
                r.version,
                r.config,
                r.stats.p50_ns as f64 / 1000.0,
                r.stats.p95_ns as f64 / 1000.0,
                r.stats.ops_per_sec,
                r.date,
                r.rerun
            );
        }
        for l in &c.excluded {
            println!(
                "  excluded: {} {}  ({}: {})",
                l.backend,
                l.version,
                l.leaf,
                l.reason.as_str()
            );
        }
    }
}
