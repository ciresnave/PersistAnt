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
use std::collections::BTreeMap;
use std::fmt;
use std::time::Instant;

/// One thing worth timing, with the contract leaf it depends on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum Workload {
    /// `put` of a 256-byte value to a fresh key.
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

/// Where and when a figure was measured. Keep every field honest: a row without its environment is a
/// claim about the present.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Environment {
    /// CPU model and logical core count, as the OS reports them.
    pub machine: String,
    /// `std::env::consts::OS`.
    pub os: String,
    /// `std::env::consts::ARCH`.
    pub arch: String,
    /// UTC date, `YYYY-MM-DD`.
    pub date: String,
    /// The server image digest, when the caller ran one (`sha256:...`).
    pub digest: Option<String>,
    /// The command that reproduces this run.
    pub rerun: String,
}

impl Environment {
    /// Read the machine, OS, architecture and date; the caller supplies the re-run command and the
    /// image digest, if any.
    pub fn detect(rerun: impl Into<String>, digest: Option<String>) -> Self {
        Environment {
            machine: machine(),
            os: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
            date: today_utc(),
            digest,
            rerun: clean(&rerun.into(), true),
        }
    }

    /// What decides whether two rows may be compared: same machine, OS and architecture.
    pub fn comparable_key(&self) -> String {
        format!("{} | {} | {}", self.machine, self.os, self.arch)
    }

    fn to_config(&self) -> Config {
        let mut c = Config::new()
            .with("arch", &self.arch)
            .with("date", &self.date)
            .with("machine", clean(&self.machine, false))
            .with("os", &self.os);
        if let Some(d) = &self.digest {
            c = c.with("digest", clean(d, false));
        }
        c
    }
}

/// Make a value safe for the data file: no tab or newline, and (inside a `k=v;k=v` field) no `;`
/// or `=`.
fn clean(s: &str, free_text: bool) -> String {
    s.chars()
        .map(|c| match c {
            '\t' | '\n' | '\r' => ' ',
            ';' | '=' if !free_text => ' ',
            c => c,
        })
        .collect::<String>()
        .trim()
        .to_string()
}

fn machine() -> String {
    let cores = std::thread::available_parallelism().map_or(0, std::num::NonZero::get);
    let model = std::env::var("PROCESSOR_IDENTIFIER")
        .ok()
        .or_else(|| {
            std::fs::read_to_string("/proc/cpuinfo").ok().and_then(|t| {
                t.lines()
                    .find(|l| l.starts_with("model name"))
                    .and_then(|l| l.split_once(':'))
                    .map(|(_, v)| v.trim().to_string())
            })
        })
        .unwrap_or_else(|| "unknown cpu".to_string());
    format!("{} x{cores}", clean(&model, false))
}

fn today_utc() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let (y, m, d) = civil_from_days(i64::try_from(secs / 86_400).unwrap_or(0));
    format!("{y:04}-{m:02}-{d:02}")
}

/// Days since 1970-01-01 to a proleptic Gregorian date (Howard Hinnant's algorithm).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = u32::try_from(doy - (153 * mp + 2) / 5 + 1).unwrap_or(1);
    let m = u32::try_from(if mp < 10 { mp + 3 } else { mp - 9 }).unwrap_or(1);
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Timing of one workload: nearest-rank percentiles of per-operation wall-clock time, in
/// nanoseconds. The clock's own resolution is coarser on some platforms; a figure near it means little.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stats {
    /// Operations timed.
    pub n: u32,
    /// Median, nanoseconds.
    pub p50_ns: u64,
    /// 95th percentile, nanoseconds.
    pub p95_ns: u64,
    /// 99th percentile, nanoseconds.
    pub p99_ns: u64,
    /// Slowest, nanoseconds.
    pub max_ns: u64,
    /// Operations per second over the summed operation time (not wall-clock including setup).
    pub ops_per_sec: u64,
}

impl Stats {
    /// From per-operation durations in nanoseconds. `None` for no samples.
    pub fn from_samples(mut us: Vec<u64>) -> Option<Self> {
        if us.is_empty() {
            return None;
        }
        us.sort_unstable();
        let n = us.len();
        let at = |p: usize| us[(n * p).div_ceil(100).saturating_sub(1).min(n - 1)];
        let total: u64 = us.iter().sum();
        Some(Stats {
            n: u32::try_from(n).unwrap_or(u32::MAX),
            p50_ns: at(50),
            p95_ns: at(95),
            p99_ns: at(99),
            max_ns: us[n - 1],
            ops_per_sec: u64::try_from(n)
                .unwrap_or(u64::MAX)
                .saturating_mul(1_000_000_000)
                .checked_div(total)
                .unwrap_or(0),
        })
    }
}

/// A figure, or the reason there is none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Timed. Only a backend that passed the contract for the workload gets this.
    Measured(Stats),
    /// Failed the contract for the workload's leaf: listed, never ranked.
    Excluded {
        /// The leaf that decided.
        leaf: String,
        /// `Missing`, `Wrong` or `Untested`.
        reason: Reason,
        /// The test's own words.
        note: String,
    },
}

/// One recorded fact: a workload on a backend in an environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpeedRow {
    /// [`Workload::id`].
    pub workload: String,
    /// Backend name, as in the capability matrix.
    pub backend: String,
    /// Backend version, as in the capability matrix.
    pub version: String,
    /// Backend configuration, as in the capability matrix.
    pub config: Config,
    /// Where and when.
    pub env: Environment,
    /// The figure or the exclusion.
    pub outcome: Outcome,
}

const HEADER: &str = "workload\tbackend\tversion\tconfig\tstatus\tn\tp50_ns\tp95_ns\tp99_ns\tmax_ns\tops_per_sec\tnote\tenv\trerun";

/// Why a speed file did not parse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError(pub String);

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "speed file: {}", self.0)
    }
}

impl std::error::Error for ParseError {}

impl SpeedRow {
    fn to_line(&self) -> String {
        let (status, nums, note) = match &self.outcome {
            Outcome::Measured(s) => (
                "measured",
                [
                    s.n.to_string(),
                    s.p50_ns.to_string(),
                    s.p95_ns.to_string(),
                    s.p99_ns.to_string(),
                    s.max_ns.to_string(),
                    s.ops_per_sec.to_string(),
                ],
                String::new(),
            ),
            Outcome::Excluded { leaf, reason, note } => (
                "excluded",
                Default::default(),
                format!("{leaf}: {}: {}", reason.as_str(), clean(note, true)),
            ),
        };
        let mut cols: Vec<String> = vec![
            self.workload.clone(),
            self.backend.clone(),
            self.version.clone(),
            self.config.to_string(),
            status.to_string(),
        ];
        cols.extend(nums);
        cols.push(note);
        cols.push(self.env.to_config().to_string());
        cols.push(self.env.rerun.clone());
        cols.join("\t")
    }

    fn from_line(line: &str) -> Result<Self, ParseError> {
        let c: Vec<&str> = line.split('\t').collect();
        if c.len() != 14 {
            return Err(ParseError(format!(
                "{} columns, want 14: {line:?}",
                c.len()
            )));
        }
        let bad = |what: &str| ParseError(format!("{what} in {line:?}"));
        let env_cfg = Config::parse(c[12]).map_err(|e| ParseError(e.to_string()))?;
        let get = |k: &str| env_cfg.get(k).map(str::to_string).ok_or_else(|| bad(k));
        let env = Environment {
            machine: get("machine")?,
            os: get("os")?,
            arch: get("arch")?,
            date: get("date")?,
            digest: env_cfg.get("digest").map(str::to_string),
            rerun: c[13].to_string(),
        };
        let num = |i: usize| c[i].parse::<u64>().map_err(|_| bad("number"));
        let outcome = match c[4] {
            "measured" => Outcome::Measured(Stats {
                n: u32::try_from(num(5)?).map_err(|_| bad("n"))?,
                p50_ns: num(6)?,
                p95_ns: num(7)?,
                p99_ns: num(8)?,
                max_ns: num(9)?,
                ops_per_sec: num(10)?,
            }),
            "excluded" => {
                let mut parts = c[11].splitn(3, ": ");
                let (leaf, reason, note) = (
                    parts.next().unwrap_or(""),
                    parts.next().unwrap_or(""),
                    parts.next().unwrap_or(""),
                );
                let reason = match reason {
                    "missing" => Reason::Missing,
                    "wrong" => Reason::Wrong,
                    "untested" => Reason::Untested,
                    _ => return Err(bad("reason")),
                };
                Outcome::Excluded {
                    leaf: leaf.to_string(),
                    reason,
                    note: note.to_string(),
                }
            }
            _ => return Err(bad("status")),
        };
        Ok(SpeedRow {
            workload: c[0].to_string(),
            backend: c[1].to_string(),
            version: c[2].to_string(),
            config: Config::parse(c[3]).map_err(|e| ParseError(e.to_string()))?,
            env,
            outcome,
        })
    }
}

/// A set of rows, read from and written to one tab-separated file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SpeedTable {
    rows: Vec<SpeedRow>,
}

impl SpeedTable {
    /// A table of `rows`.
    pub fn from_rows(rows: Vec<SpeedRow>) -> Self {
        SpeedTable { rows }
    }

    /// The rows.
    pub fn rows(&self) -> &[SpeedRow] {
        &self.rows
    }

    /// Add the rows of another file (a different backend, maybe a different machine).
    pub fn merge(&mut self, other: SpeedTable) {
        self.rows.extend(other.rows);
    }

    /// The file text.
    pub fn to_tsv(&self) -> String {
        let mut out = String::from(HEADER);
        out.push('\n');
        for r in &self.rows {
            out.push_str(&r.to_line());
            out.push('\n');
        }
        out
    }

    /// Parse a file written by [`to_tsv`](Self::to_tsv).
    pub fn parse(text: &str) -> Result<Self, ParseError> {
        let mut lines = text.lines();
        if lines.next() != Some(HEADER) {
            return Err(ParseError("missing or wrong header".to_string()));
        }
        let rows = lines
            .filter(|l| !l.is_empty())
            .map(SpeedRow::from_line)
            .collect::<Result<_, _>>()?;
        Ok(SpeedTable { rows })
    }
}

/// Time `workloads` on `target`, `iterations` operations each. A workload whose leaf the target does
/// not pass gets an excluded row and no timing.
pub async fn run<K: KvBackend>(
    target: &K,
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
                    leaf: e.leaf.id().to_string(),
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

const VALUE_BYTES: usize = 256;

async fn time<K: KvBackend>(
    b: &K,
    w: Workload,
    iterations: u32,
) -> Result<Option<Stats>, crate::KvError> {
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

/// A backend that has a number for a workload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ranked {
    /// Backend name.
    pub backend: String,
    /// Backend version.
    pub version: String,
    /// Backend configuration.
    pub config: Config,
    /// The measurement.
    pub stats: Stats,
    /// The date of the measurement.
    pub date: String,
    /// The command that reproduces it.
    pub rerun: String,
}

/// A backend that has no number for a workload, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Left {
    /// Backend name.
    pub backend: String,
    /// Backend version.
    pub version: String,
    /// The leaf that excluded it.
    pub leaf: String,
    /// Why.
    pub reason: Reason,
}

/// One workload in one environment: the eligible backends fastest first by median, then the excluded
/// ones. The environment is part of the answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Comparison {
    /// [`Workload::id`].
    pub workload: String,
    /// [`Environment::comparable_key`]: this ordering holds here and nowhere else.
    pub environment: String,
    /// Backends with a number, ascending by `p50_ns` (ties by name).
    pub ranked: Vec<Ranked>,
    /// Backends listed without a number.
    pub excluded: Vec<Left>,
}

/// Group rows by workload and environment and order the measured ones. Rows from different machines
/// never share a [`Comparison`].
pub fn compare(table: &SpeedTable) -> Vec<Comparison> {
    let mut groups: BTreeMap<(String, String), Comparison> = BTreeMap::new();
    for r in table.rows() {
        let key = (r.workload.clone(), r.env.comparable_key());
        let g = groups.entry(key.clone()).or_insert_with(|| Comparison {
            workload: key.0.clone(),
            environment: key.1.clone(),
            ranked: Vec::new(),
            excluded: Vec::new(),
        });
        match &r.outcome {
            Outcome::Measured(s) => g.ranked.push(Ranked {
                backend: r.backend.clone(),
                version: r.version.clone(),
                config: r.config.clone(),
                stats: *s,
                date: r.env.date.clone(),
                rerun: r.env.rerun.clone(),
            }),
            Outcome::Excluded { leaf, reason, .. } => g.excluded.push(Left {
                backend: r.backend.clone(),
                version: r.version.clone(),
                leaf: leaf.clone(),
                reason: *reason,
            }),
        }
    }
    let mut out: Vec<Comparison> = groups.into_values().collect();
    for g in &mut out {
        g.ranked.sort_by(|a, b| {
            (a.stats.p50_ns, &a.backend, &a.version).cmp(&(b.stats.p50_ns, &b.backend, &b.version))
        });
    }
    out
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

    #[test]
    fn percentiles_are_nearest_rank_and_an_empty_sample_has_none() {
        assert!(Stats::from_samples(vec![]).is_none());
        let s = Stats::from_samples((1..=100).collect()).unwrap();
        assert_eq!(
            (s.n, s.p50_ns, s.p95_ns, s.p99_ns, s.max_ns),
            (100, 50, 95, 99, 100)
        );
        let one = Stats::from_samples(vec![7]).unwrap();
        assert_eq!((one.p50_ns, one.p99_ns, one.max_ns), (7, 7, 7));
    }

    #[test]
    fn dates_are_civil_utc() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(19_000), (2022, 1, 8));
        assert_eq!(civil_from_days(20_742), (2026, 10, 16));
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

    fn measured(backend: &str, machine: &str, p50: u64) -> SpeedRow {
        SpeedRow {
            workload: Workload::Get.id().to_string(),
            backend: backend.to_string(),
            version: "1".to_string(),
            config: Config::new(),
            env: env(machine),
            outcome: Outcome::Measured(Stats::from_samples(vec![p50; 10]).unwrap()),
        }
    }

    #[test]
    fn rankings_never_cross_machines_and_excluded_backends_are_listed_not_ranked() {
        let mut rows = vec![
            measured("slow", "laptop", 900),
            measured("fast", "laptop", 20),
            // On another machine the order is the opposite: it must be a separate comparison.
            measured("slow", "server", 5),
            measured("fast", "server", 50),
        ];
        rows.push(SpeedRow {
            outcome: Outcome::Excluded {
                leaf: "kv.get".to_string(),
                reason: Reason::Wrong,
                note: "lost a write".to_string(),
            },
            ..measured("broken", "laptop", 1)
        });
        let cmp = compare(&SpeedTable::from_rows(rows));
        assert_eq!(cmp.len(), 2, "one comparison per environment");
        let laptop = cmp
            .iter()
            .find(|c| c.environment.starts_with("laptop"))
            .unwrap();
        let names: Vec<_> = laptop.ranked.iter().map(|r| r.backend.as_str()).collect();
        assert_eq!(names, ["fast", "slow"]);
        assert_eq!(laptop.excluded.len(), 1);
        assert_eq!(laptop.excluded[0].backend, "broken");
        let server = cmp
            .iter()
            .find(|c| c.environment.starts_with("server"))
            .unwrap();
        let names: Vec<_> = server.ranked.iter().map(|r| r.backend.as_str()).collect();
        assert_eq!(names, ["slow", "fast"]);
    }

    #[test]
    fn a_malformed_file_is_an_error_not_an_empty_table() {
        assert!(SpeedTable::parse("nope").is_err());
        assert!(SpeedTable::parse(&format!("{HEADER}\ntoo\tfew")).is_err());
    }
}
