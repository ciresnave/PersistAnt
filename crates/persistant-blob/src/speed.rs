// SPDX-License-Identifier: MIT OR Apache-2.0
//! Stage 3 of backend benchmarking for the blob kind: **speed**, as data, with the environment
//! recorded. The rules and the data types are the kind-agnostic ones in [`persistant_core::bench`]
//! (only a backend that passes the contract leaf a workload depends on is timed; the rest are listed
//! as excluded, never ranked; a figure carries its environment and the command that re-runs it).
//! This module owns the blob workloads and how they are timed: wall-clock, one operation at a time,
//! from the caller's side of the trait.

use crate::conformance::Leaf;
use crate::contract;
use crate::{BlobBackend, BlobError};
use persistant_core::choose::Eligibility;
use persistant_core::{Config, Reason};
use std::time::Instant;

pub use persistant_core::bench::{
    Comparison, Environment, Left, Outcome, Position, Profile, Ranked,
    SpeedParseError as ParseError, SpeedRow, SpeedTable, Stats, compare, compare_across,
};

/// One thing worth timing, with the contract leaf it depends on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum Workload {
    /// `write` of a 4 KiB value to a fresh key.
    Write,
    /// `read` of one present 4 KiB value.
    Read,
    /// `stat` of one present key.
    Stat,
    /// `list` pages of 50 entries over 100 keys.
    List,
    /// `create_exclusive` of a fresh key (the claim succeeds).
    CreateExclusive,
    /// `replace_atomic` of one key.
    ReplaceAtomic,
    /// `append` of 256 bytes to one key.
    Append,
}

impl Workload {
    /// Every workload.
    pub const ALL: [Workload; 7] = [
        Workload::Write,
        Workload::Read,
        Workload::Stat,
        Workload::List,
        Workload::CreateExclusive,
        Workload::ReplaceAtomic,
        Workload::Append,
    ];

    /// The id used in data files.
    pub fn id(self) -> &'static str {
        match self {
            Workload::Write => "speed.blob.write",
            Workload::Read => "speed.blob.read",
            Workload::Stat => "speed.blob.stat",
            Workload::List => "speed.blob.list",
            Workload::CreateExclusive => "speed.blob.create_exclusive",
            Workload::ReplaceAtomic => "speed.blob.replace_atomic",
            Workload::Append => "speed.blob.append",
        }
    }

    /// The contract leaf the workload needs: no pass, no number.
    pub fn required(self) -> Leaf {
        match self {
            Workload::Write => Leaf::Write,
            Workload::Read => Leaf::Read,
            Workload::Stat => Leaf::Stat,
            Workload::List => Leaf::List,
            Workload::CreateExclusive => Leaf::CreateExclusive,
            Workload::ReplaceAtomic => Leaf::ReplaceConcurrentRead,
            Workload::Append => Leaf::Append,
        }
    }
}

/// Time `workloads` on `target`, `iterations` operations each. A workload whose leaf the target does
/// not pass gets an excluded row and no timing.
pub async fn run<B: BlobBackend>(
    target: &B,
    config: &Config,
    env: &Environment,
    workloads: &[Workload],
    iterations: u32,
) -> Vec<SpeedRow> {
    let mut out = Vec::with_capacity(workloads.len());
    for w in workloads.iter().copied() {
        let leaf = w.required();
        let outcome = match contract::check(target, &[leaf]).await {
            Eligibility::Eligible => match time(target, w, iterations).await {
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
                    leaf: e.leaf.clone(),
                    reason: e.reason,
                    note: e.note.clone(),
                }
            }
        };
        out.push(SpeedRow {
            workload: w.id().to_string(),
            backend: target.backend().to_string(),
            version: target.version().to_string(),
            config: config.clone(),
            env: env.clone(),
            outcome,
        });
    }
    out
}

const VALUE_BYTES: usize = 4096;

async fn time<B: BlobBackend>(
    b: &B,
    w: Workload,
    iterations: u32,
) -> Result<Option<Stats>, BlobError> {
    let base = format!("persistant-speed/{}/", w.id());
    let value = vec![0x5A_u8; VALUE_BYTES];
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
        Workload::Write | Workload::CreateExclusive => {
            for i in 0..iterations {
                let k = format!("{base}{i:08}");
                keys.push(k.clone());
                if w == Workload::Write {
                    timed!(b.write(&k, value.clone()).await?);
                } else {
                    let won = timed!(b.create_exclusive(&k, value.clone()).await?);
                    if !won {
                        return Err(BlobError::Backend(
                            "create_exclusive on a fresh key did not create".to_string(),
                        ));
                    }
                }
            }
        }
        Workload::Read | Workload::Stat | Workload::ReplaceAtomic | Workload::Append => {
            let k = format!("{base}k");
            keys.push(k.clone());
            b.write(&k, value.clone()).await?;
            for _ in 0..iterations {
                match w {
                    Workload::Read => {
                        timed!(b.read(&k).await?);
                    }
                    Workload::Stat => {
                        timed!(b.stat(&k).await?);
                    }
                    Workload::ReplaceAtomic => timed!(b.replace_atomic(&k, value.clone()).await?),
                    _ => timed!(b.append(&k, value[..256].to_vec()).await?),
                }
            }
        }
        Workload::List => {
            for i in 0..100 {
                let k = format!("{base}{i:04}");
                keys.push(k.clone());
                b.write(&k, value[..256].to_vec()).await?;
            }
            for _ in 0..iterations {
                timed!(b.list(&base, None, 50).await?);
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
    use crate::{Defect, MemoryBlob};

    fn env() -> Environment {
        Environment::detect("cargo test -p persistant-blob", None)
    }

    #[tokio::test]
    async fn a_correct_backend_is_measured_for_every_workload_and_the_file_round_trips() {
        let rows = run(
            &MemoryBlob::new(),
            &Config::new(),
            &env(),
            &Workload::ALL,
            20,
        )
        .await;
        assert_eq!(rows.len(), Workload::ALL.len());
        for r in &rows {
            let Outcome::Measured(s) = &r.outcome else {
                panic!("{}: {:?}", r.workload, r.outcome)
            };
            assert_eq!(s.n, 20, "{}", r.workload);
            assert!(s.p50_ns <= s.p95_ns && s.p95_ns <= s.p99_ns && s.p99_ns <= s.max_ns);
        }
        let t = SpeedTable::from_rows(rows);
        assert_eq!(SpeedTable::parse(&t.to_tsv()).unwrap(), t);
    }

    #[tokio::test]
    async fn a_backend_that_fails_the_contract_gets_no_number() {
        let lossy = MemoryBlob::with_defect(Defect::WriteIsLost);
        let rows = run(&lossy, &Config::new(), &env(), &[Workload::Write], 5).await;
        assert!(
            matches!(
                &rows[0].outcome,
                Outcome::Excluded {
                    reason: Reason::Wrong,
                    ..
                }
            ),
            "{:?}",
            rows[0].outcome
        );
        // Control: the same workload on a correct backend is measured.
        let rows = run(
            &MemoryBlob::new(),
            &Config::new(),
            &env(),
            &[Workload::Write],
            5,
        )
        .await;
        assert!(matches!(rows[0].outcome, Outcome::Measured(_)));
    }
}
