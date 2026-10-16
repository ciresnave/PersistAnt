// SPDX-License-Identifier: MIT OR Apache-2.0
//! The kind-agnostic half of backend benchmarking stage 3 (speed): the environment a figure was taken
//! in, timing statistics, the data file, and the comparison that never crosses machines. The kinds
//! (`persistant_kv::speed`, `persistant_blob::speed`) own their workloads and how they are timed; they
//! produce [`SpeedRow`]s and nothing here knows what a key-value store or a blob store is.
//!
//! - **Speed ranks only what passes the contract.** A row is either [`Outcome::Measured`] or
//!   [`Outcome::Excluded`] with the leaf and reason; an excluded backend is listed, never ranked.
//! - **No constant rankings.** Every row carries its [`Environment`] and the command that re-runs it;
//!   [`compare`] orders backends only *within one environment*.
//! - **Results are data files.** [`SpeedTable`] reads and writes a tab-separated file.

use crate::{Config, Reason};
use std::collections::BTreeMap;
use std::fmt;

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
pub struct SpeedParseError(pub String);

impl fmt::Display for SpeedParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "speed file: {}", self.0)
    }
}

impl std::error::Error for SpeedParseError {}

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

    fn from_line(line: &str) -> Result<Self, SpeedParseError> {
        let c: Vec<&str> = line.split('\t').collect();
        if c.len() != 14 {
            return Err(SpeedParseError(format!(
                "{} columns, want 14: {line:?}",
                c.len()
            )));
        }
        let bad = |what: &str| SpeedParseError(format!("{what} in {line:?}"));
        let env_cfg = Config::parse(c[12]).map_err(|e| SpeedParseError(e.to_string()))?;
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
            config: Config::parse(c[3]).map_err(|e| SpeedParseError(e.to_string()))?,
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
    pub fn parse(text: &str) -> Result<Self, SpeedParseError> {
        let mut lines = text.lines();
        if lines.next() != Some(HEADER) {
            return Err(SpeedParseError("missing or wrong header".to_string()));
        }
        let rows = lines
            .filter(|l| !l.is_empty())
            .map(SpeedRow::from_line)
            .collect::<Result<_, _>>()?;
        Ok(SpeedTable { rows })
    }
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

    fn measured(backend: &str, machine: &str, p50: u64) -> SpeedRow {
        SpeedRow {
            workload: "speed.kv.get".to_string(),
            backend: backend.to_string(),
            version: "1".to_string(),
            config: Config::new(),
            env: env(machine),
            outcome: Outcome::Measured(Stats::from_samples(vec![p50; 10]).unwrap()),
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

    #[test]
    fn a_table_round_trips_measured_and_excluded_rows() {
        let mut rows = vec![measured("a", "m", 5)];
        rows.push(SpeedRow {
            outcome: Outcome::Excluded {
                leaf: "kv.get".to_string(),
                reason: Reason::Wrong,
                note: "lost a write".to_string(),
            },
            ..measured("b", "m", 1)
        });
        let t = SpeedTable::from_rows(rows);
        assert_eq!(SpeedTable::parse(&t.to_tsv()).unwrap(), t);
    }

    #[test]
    fn a_malformed_file_is_an_error_not_an_empty_table() {
        assert!(SpeedTable::parse("nope").is_err());
        assert!(SpeedTable::parse(&format!("{HEADER}\ntoo\tfew")).is_err());
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
}
