// SPDX-License-Identifier: MIT OR Apache-2.0
//! What `SqliteBackend::open_secure_wal` costs: single-row autocommit inserts per second on a fresh
//! file for three profiles, interleaved round by round so a loaded box hits them alike. Prints
//! the median rate of each profile and the median ratio to `open_durable` (WAL, no secure delete).
//! `cargo run --release -p persistant-sqlx --features sqlite --example secure_wal_cost`

use persistant_relational::{RelationalBackend, Value};
use persistant_sqlx::SqliteBackend;
use std::time::Instant;

const ROUNDS: usize = 9;
const WRITES: i64 = 500;

async fn rate(profile: &str, dir: &std::path::Path) -> f64 {
    let path = dir.join(format!("{profile}.sqlite"));
    let db = match profile {
        "wal" => SqliteBackend::open_durable(&path).await.unwrap(),
        "secure-journal" => SqliteBackend::open_secure(&path).await.unwrap(),
        "secure-wal" => SqliteBackend::open_secure_wal(&path).await.unwrap(),
        _ => unreachable!(),
    };
    db.execute("CREATE TABLE t (k INTEGER PRIMARY KEY, v BLOB)", &[])
        .await
        .unwrap();
    let t = Instant::now();
    for i in 0..WRITES {
        db.execute(
            "INSERT INTO t VALUES ($1, $2)",
            &[Value::Int(i), Value::Bytes(vec![b'x'; 200])],
        )
        .await
        .unwrap();
    }
    WRITES as f64 / t.elapsed().as_secs_f64()
}

fn median(v: &mut [f64]) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[v.len() / 2]
}

#[tokio::main]
async fn main() {
    let profiles = ["wal", "secure-journal", "secure-wal"];
    let mut rates: Vec<Vec<f64>> = vec![Vec::new(); 3];
    for round in 0..=ROUNDS {
        let dir =
            std::env::temp_dir().join(format!("securewalcost-{}-{round}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for (i, p) in profiles.iter().enumerate() {
            let r = rate(p, &dir).await;
            if round > 0 {
                rates[i].push(r); // round 0 is the warm-up, discarded
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
    println!(
        "{WRITES} single-row autocommit inserts per profile per round, {ROUNDS} rounds (warm-up discarded)"
    );
    let base = rates[0].clone();
    for (i, p) in profiles.iter().enumerate() {
        let mut ratios: Vec<f64> = rates[i].iter().zip(&base).map(|(a, b)| a / b).collect();
        let (lo, hi) = (
            ratios.iter().cloned().fold(f64::MAX, f64::min),
            ratios.iter().cloned().fold(f64::MIN, f64::max),
        );
        println!(
            "{p:15} median {:9.0} writes/s   ratio to wal: median {:.2} (min {lo:.2}, max {hi:.2})",
            median(&mut rates[i].clone()),
            median(&mut ratios)
        );
    }
}
