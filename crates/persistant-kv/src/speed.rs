// SPDX-License-Identifier: MIT OR Apache-2.0
//! Stage 3 of backend benchmarking: **speed**, as data, with the environment recorded.
//!
//! Rules this module is built around:
//!
//! - **Speed ranks only what passes the contract.** [`run`] first asks [`contract::check`] for the
//!   leaf each workload depends on. A backend that fails it gets an [`Outcome::Excluded`] row with
//!   the reason: it is listed, never given a number, never ranked.
//! - **No constant rankings.** A [`SpeedRow`] carries its [`Environment`] (machine, OS, architecture,
//!   date, the server's image digest if the caller has one) and the command that re-runs it.
//!   [`compare`] orders backends only *within one environment*; rows from different machines are
//!   never put in one list.
//! - **Results are data files.** [`SpeedTable`] reads and writes a tab-separated file, one row per
//!   `(workload, backend, config, environment)`, like the capability matrix.
//!
//! Timing is wall-clock, one operation at a time, from the caller's side of the trait (so a network
//! database includes its round trip, which is the cost a consumer pays). Percentiles are nearest-rank.
//! A figure is a measurement of that run on that machine, not a property of the backend.

use crate::conformance::Leaf;
use crate::contract::{self, Eligibility};
use crate::{BatchOp, KvBackend};
use persistant_core::{Config, Reason};
use std::time::Instant;

/// One thing worth timing, with the contract leaf it depends on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum Workload {
    /// `put` of a 256-byte value (or the size given to [`run_sized`]) to a fresh key.
    Put,
    /// `get` of one present key.
    Get,
    /// `put_if_absent` of a fresh key (the claim succeeds).
    PutIfAbsent,
    /// `compare_and_swap` of one key, each swap expecting the last value.
    CompareAndSwap,
    /// `batch` of two `Put`s guarded by a `Require`-absent on a fresh key.
    Batch,
    /// `ordered_scan` pages of 50 keys over 100 keys.
    OrderedScan,
    /// `ordered_scan_values` pages of 50 entries over 100 keys.
    ScanValues,
}

impl Workload {
    /// Every workload.
    pub const ALL: [Workload; 7] = [
        Workload::Put,
        Workload::Get,
        Workload::PutIfAbsent,
        Workload::CompareAndSwap,
        Workload::Batch,
        Workload::OrderedScan,
        Workload::ScanValues,
    ];

    /// The id used in data files.
    pub fn id(self) -> &'static str {
        match self {
            Workload::Put => "speed.kv.put",
            Workload::Get => "speed.kv.get",
            Workload::PutIfAbsent => "speed.kv.put_if_absent",
            Workload::CompareAndSwap => "speed.kv.compare_and_swap",
            Workload::Batch => "speed.kv.batch",
            Workload::OrderedScan => "speed.kv.ordered_scan",
            Workload::ScanValues => "speed.kv.ordered_scan_values",
        }
    }

    /// The id of this workload at `value_bytes`-byte values. The default size
    /// ([`DEFAULT_VALUE_BYTES`]) keeps the plain [`id`](Workload::id), so existing data files stay
    /// valid; any other size gets a `.v<bytes>` suffix. The size is part of the id, so [`compare`]
    /// never ranks backends measured at different sizes in one list.
    pub fn id_at(self, value_bytes: usize) -> String {
        if value_bytes == DEFAULT_VALUE_BYTES {
            self.id().to_string()
        } else {
            format!("{}.v{value_bytes}", self.id())
        }
    }

    /// The contract leaf the workload needs: no pass, no number.
    pub fn required(self) -> Leaf {
        match self {
            Workload::Put => Leaf::Put,
            Workload::Get => Leaf::Get,
            Workload::PutIfAbsent => Leaf::PutIfAbsent,
            Workload::CompareAndSwap => Leaf::CompareAndSwap,
            Workload::Batch => Leaf::BatchRollback,
            Workload::OrderedScan => Leaf::OrderedScan,
            Workload::ScanValues => Leaf::ScanValues,
        }
    }
}

pub use persistant_core::bench::{
    Comparison, Environment, Left, Outcome, Position, Profile, Ranked,
    SpeedParseError as ParseError, SpeedRow, SpeedTable, Stats, compare, compare_across,
};

/// The value size, in bytes, [`run`] times with.
pub const DEFAULT_VALUE_BYTES: usize = 256;

/// Time `workloads` on `target`, `iterations` operations each, with [`DEFAULT_VALUE_BYTES`]-byte
/// values. A workload whose leaf the target does not pass gets an excluded row and no timing.
pub async fn run<K: KvBackend>(
    target: &K,
    config: &Config,
    env: &Environment,
    workloads: &[Workload],
    iterations: u32,
) -> Vec<SpeedRow> {
    run_sized(
        target,
        config,
        env,
        workloads,
        iterations,
        DEFAULT_VALUE_BYTES,
    )
    .await
}

/// [`run`] with `value_bytes`-byte values (at least 1). Rows carry [`Workload::id_at`], so each size
/// is its own workload in the data file.
pub async fn run_sized<K: KvBackend>(
    target: &K,
    config: &Config,
    env: &Environment,
    workloads: &[Workload],
    iterations: u32,
    value_bytes: usize,
) -> Vec<SpeedRow> {
    let value_bytes = value_bytes.max(1);
    let mut out = Vec::with_capacity(workloads.len());
    for w in workloads.iter().copied() {
        let leaf = w.required();
        let outcome = match contract::check(target, &[leaf]).await {
            Eligibility::Eligible => match time(target, w, iterations, value_bytes).await {
                Ok(Some(s)) => Outcome::Measured(s),
                Ok(None) => Outcome::Excluded {
                    leaf: leaf.id().to_string(),
                    reason: Reason::Untested,
                    note: "no operations were timed".to_string(),
                },
                Err(e) => Outcome::Excluded {
                    leaf: leaf.id().to_string(),
                    reason: Reason::Missing,
                    note: format!("the workload failed on a backend that passed the leaf: {e}"),
                },
            },
            Eligibility::Excluded(ex) => {
                let e = &ex[0];
                Outcome::Excluded {
                    leaf: e.leaf.id().to_string(),
                    reason: e.reason,
                    note: e.note.clone(),
                }
            }
        };
        out.push(SpeedRow {
            workload: w.id_at(value_bytes),
            backend: target.backend().to_string(),
            version: target.version().to_string(),
            config: config.clone(),
            env: env.clone(),
            outcome,
        });
    }
    out
}

async fn time<K: KvBackend>(
    b: &K,
    w: Workload,
    iterations: u32,
    value_bytes: usize,
) -> Result<Option<Stats>, crate::KvError> {
    let base = format!("persistant-speed/{}/", w.id());
    let value = vec![0x5A_u8; value_bytes];
    let mut us: Vec<u64> = Vec::with_capacity(iterations as usize);
    let mut keys: Vec<String> = Vec::new();
    macro_rules! timed {
        ($e:expr) => {{
            let t = Instant::now();
            let r = $e;
            us.push(u64::try_from(t.elapsed().as_nanos()).unwrap_or(u64::MAX));
            r
        }};
    }
    match w {
        Workload::Put | Workload::PutIfAbsent | Workload::Batch => {
            for i in 0..iterations {
                let k = format!("{base}{i:08}");
                keys.push(k.clone());
                match w {
                    Workload::Put => timed!(b.put(&k, value.clone(), None).await?),
                    Workload::PutIfAbsent => {
                        let won = timed!(b.put_if_absent(&k, value.clone(), None).await?);
                        if !won {
                            return Err(crate::KvError::Backend(
                                "put_if_absent on a fresh key did not store".to_string(),
                            ));
                        }
                    }
                    _ => {
                        let second = format!("{k}b");
                        keys.push(second.clone());
                        let applied = timed!(
                            b.batch(&[
                                BatchOp::require(&k, None),
                                BatchOp::put(&k, value.clone()),
                                BatchOp::put(&second, value.clone()),
                            ])
                            .await?
                        );
                        if !applied {
                            return Err(crate::KvError::Backend(
                                "a batch guarded by Require-absent on a fresh key was refused"
                                    .to_string(),
                            ));
                        }
                    }
                }
            }
        }
        Workload::Get => {
            let k = format!("{base}k");
            keys.push(k.clone());
            b.put(&k, value.clone(), None).await?;
            for _ in 0..iterations {
                timed!(b.get(&k).await?);
            }
        }
        Workload::CompareAndSwap => {
            let k = format!("{base}k");
            keys.push(k.clone());
            let mut cur = value.clone();
            b.put(&k, cur.clone(), None).await?;
            for i in 0..iterations {
                let mut next = value.clone();
                next[0] = (i % 251) as u8 + 1;
                let swapped = timed!(b.compare_and_swap(&k, &cur, next.clone(), None).await?);
                if !swapped {
                    return Err(crate::KvError::Backend(
                        "a swap expecting the current value was refused".to_string(),
                    ));
                }
                cur = next;
            }
        }
        Workload::OrderedScan | Workload::ScanValues => {
            for i in 0..100 {
                let k = format!("{base}{i:04}");
                keys.push(k.clone());
                b.put(&k, value.clone(), None).await?;
            }
            for _ in 0..iterations {
                if w == Workload::OrderedScan {
                    timed!(b.ordered_scan(&base, None, 50).await?);
                } else {
                    timed!(b.ordered_scan_values(&base, None, 50).await?);
                }
            }
        }
    }
    for k in &keys {
        let _ = b.delete(k).await; // best effort: leave a shared target as found
    }
    Ok(Stats::from_samples(us))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Defect, KvOnBlob, MemoryKv};
    use persistant_blob::MemoryBlob;

    fn env(machine: &str) -> Environment {
        Environment {
            machine: machine.to_string(),
            os: "linux".to_string(),
            arch: "x86_64".to_string(),
            date: "2026-10-16".to_string(),
            digest: Some("sha256:abc".to_string()),
            rerun: "cargo run --example speed".to_string(),
        }
    }

    #[tokio::test]
    async fn a_correct_backend_is_measured_for_every_workload_and_the_file_round_trips() {
        let e = Environment::detect("cargo run --example speed", None);
        let rows = run(&MemoryKv::new(), &Config::new(), &e, &Workload::ALL, 20).await;
        assert_eq!(rows.len(), Workload::ALL.len());
        for r in &rows {
            let Outcome::Measured(s) = &r.outcome else {
                panic!("{}: {:?}", r.workload, r.outcome)
            };
            assert_eq!(s.n, 20);
            assert!(s.p50_ns <= s.p95_ns && s.p95_ns <= s.p99_ns && s.p99_ns <= s.max_ns);
        }
        let t = SpeedTable::from_rows(rows);
        assert_eq!(SpeedTable::parse(&t.to_tsv()).unwrap(), t);
    }

    #[tokio::test]
    async fn value_size_is_part_of_the_workload_id_and_never_shares_a_ranking() {
        let e = env("m");
        let kv = MemoryKv::new();
        let small = run_sized(&kv, &Config::new(), &e, &[Workload::Put], 5, 64).await;
        let default = run(&kv, &Config::new(), &e, &[Workload::Put], 5).await;
        let big = run_sized(&kv, &Config::new(), &e, &[Workload::Put], 5, 4096).await;
        assert_eq!(default[0].workload, "speed.kv.put");
        assert_eq!(small[0].workload, "speed.kv.put.v64");
        assert_eq!(big[0].workload, "speed.kv.put.v4096");
        let mut rows = small;
        rows.extend(default);
        rows.extend(big);
        let t = SpeedTable::from_rows(rows);
        let cmp = compare(&t);
        // Control: one backend at one size is one comparison; three sizes are three.
        assert_eq!(cmp.len(), 3);
        assert!(cmp.iter().all(|c| c.ranked.len() == 1));
        assert_eq!(SpeedTable::parse(&t.to_tsv()).unwrap(), t);
    }

    #[tokio::test]
    async fn a_backend_that_fails_the_contract_gets_no_number() {
        // No batch on a blob store: excluded as Missing, not timed.
        let e = env("m");
        let kv = KvOnBlob::new(MemoryBlob::new());
        let rows = run(
            &kv,
            &Config::new(),
            &e,
            &[Workload::Put, Workload::Batch],
            5,
        )
        .await;
        assert!(matches!(rows[0].outcome, Outcome::Measured(_)));
        assert!(
            matches!(
                &rows[1].outcome,
                Outcome::Excluded {
                    reason: Reason::Missing,
                    ..
                }
            ),
            "{:?}",
            rows[1].outcome
        );
        // A backend that loses puts is Wrong for put, whatever its speed.
        let lossy = MemoryKv::with_defect(Defect::PutIsLost);
        let rows = run(&lossy, &Config::new(), &e, &[Workload::Put], 5).await;
        assert!(matches!(
            &rows[0].outcome,
            Outcome::Excluded {
                reason: Reason::Wrong,
                ..
            }
        ));
        // Control: the same workload on a correct backend is measured.
        let rows = run(&MemoryKv::new(), &Config::new(), &e, &[Workload::Put], 5).await;
        assert!(matches!(rows[0].outcome, Outcome::Measured(_)));
    }
}
