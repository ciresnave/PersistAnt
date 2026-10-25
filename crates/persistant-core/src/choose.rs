// SPDX-License-Identifier: MIT OR Apache-2.0
//! Stage 4 of backend benchmarking, kind-agnostic: **"best backend for X"**, as a ranked list *with its
//! evidence*. The kinds (`persistant_kv::choose`, `persistant_blob::choose`) supply their contract leaf
//! ids, their workload ids and a table of what their backends declare about durability; the logic is
//! here, once.
//!
//! The consumer states what it needs; nothing is chosen for it:
//!
//! - the contract leaves it relies on ([`Need::leaves`]),
//! - the **minimum durability class** it will accept ([`Need::min_durability`]),
//! - the one workload it ranks by ([`Need::rank_by`]), and
//! - the environment the ranking is for ([`Need::environment`], an
//!   [`Environment::comparable_key`](crate::bench::Environment::comparable_key)).
//!
//! [`choose`] returns a [`Choice`]: the eligible backends fastest first, each with the measurement it
//! was ranked on (date, re-run command, image digest), the eligible ones that were not measured in that
//! environment, and every backend that was **excluded, with the reason**. If nothing is eligible it is
//! an error ([`NoneEligible`]) that carries the exclusions: no silent fallback, no "closest match".
//! Every decision is written as lines ([`Choice::log_lines`]) the caller logs.
//!
//! **Durability is declared; `Process` and `Machine` are verified only where a test ran** (the kind's
//! process-kill test; the guest power-cut harness in `scripts/machine-durability`). A backend with no
//! declaration is excluded as unknown once the need asks for more than `Volatile`. `Machine` is marked
//! verified only for what that harness exercised, and never for host power loss or NTFS.

use crate::bench::{Outcome, SpeedTable, Stats};
use crate::{Config, Reason};
use std::fmt;

/// A required leaf the backend did not pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeafFailure {
    /// The leaf id (`kv.get`).
    pub leaf: String,
    /// `Missing`, `Wrong` or `Untested`.
    pub reason: Reason,
    /// The test's own words.
    pub note: String,
}

/// Whether a backend passed the contract for the leaves a need names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Eligibility {
    /// Every needed leaf passed.
    Eligible,
    /// At least one did not. Never empty.
    Excluded(Vec<LeafFailure>),
}

/// How much a stored value survives. Ordered: a higher class includes the lower ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum Durability {
    /// Lost when the process exits.
    Volatile,
    /// Survives the process exiting or crashing; not claimed to survive the machine losing power.
    Process,
    /// Documented to survive an operating-system crash or power loss (the write is flushed to stable
    /// storage before it is acknowledged).
    Machine,
}

impl Durability {
    /// The word used in logs.
    pub fn as_str(self) -> &'static str {
        match self {
            Durability::Volatile => "volatile",
            Durability::Process => "process",
            Durability::Machine => "machine",
        }
    }
}

/// A durability class, where it comes from, and how much of it a test has checked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Declared {
    /// The class the backend documents. Eligibility uses this.
    pub class: Durability,
    /// Why it is believed.
    pub basis: &'static str,
    /// The highest class a test has actually verified, if any. `Process` is verified by
    /// [`durability`](crate::durability) (`kv.durability.process`: a writer killed after
    /// acknowledged writes, the store opened again). `Machine` is verified by `kv.durability.machine`
    /// (`scripts/machine-durability`: a Hyper-V guest powered off after acknowledged writes, booted, every
    /// acknowledged key checked), which says nothing about host power loss, NTFS or the disk's own cache.
    pub verified: Option<Durability>,
}

/// What the consumer needs. Every field is the consumer's to state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Need {
    /// The contract leaves the consumer relies on. A backend failing any is excluded.
    pub leaves: Vec<String>,
    /// The lowest durability class acceptable.
    pub min_durability: Durability,
    /// The workload to order eligible backends by (median time, smaller first).
    pub rank_by: String,
    /// [`Environment::comparable_key`](crate::bench::Environment::comparable_key) of the
    /// machine the ranking is for. Measurements from any other environment are not used.
    pub environment: String,
}

/// One backend, with the evidence gathered about it. `eligibility` comes from the kind's contract
/// check on the needed leaves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// Backend name, as in the matrix and the speed files.
    pub backend: String,
    /// Backend version.
    pub version: String,
    /// Backend configuration.
    pub config: Config,
    /// The result of the contract check for the needed leaves.
    pub eligibility: Eligibility,
}

/// Why a backend is not in the ranking.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Why {
    /// Failed (or was never tested on) a needed leaf.
    Contract(Vec<LeafFailure>),
    /// Its declared durability is below the minimum.
    Durability {
        /// What it declares.
        declared: Durability,
        /// What was asked for.
        wanted: Durability,
    },
    /// No durability declaration exists, and the need asks for more than `Volatile`.
    DurabilityUnknown {
        /// What was asked for.
        wanted: Durability,
    },
}

/// A backend left out, with the reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Excluded {
    /// Backend name.
    pub backend: String,
    /// Backend version.
    pub version: String,
    /// Why.
    pub why: Why,
}

/// A backend in the ranking, with what it was ranked on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Backend name.
    pub backend: String,
    /// Backend version.
    pub version: String,
    /// Backend configuration.
    pub config: Config,
    /// The measurement it was ranked by.
    pub stats: Stats,
    /// When it was measured.
    pub date: String,
    /// The image digest of the server, if it ran in a container.
    pub digest: Option<String>,
    /// The command that reproduces the measurement.
    pub rerun: String,
    /// Its declared durability and the basis.
    pub durability: Option<Declared>,
}

/// An eligible backend with no measurement of the ranking workload in the needed environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unmeasured {
    /// Backend name.
    pub backend: String,
    /// Backend version.
    pub version: String,
}

/// The answer: ranked, with evidence, and everything left out with its reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    /// The need this answers, so the log line is self-contained.
    pub need: Need,
    /// Eligible and measured, fastest median first (ties by name, then version).
    pub ranked: Vec<Entry>,
    /// Eligible but not measured in this environment: not ranked, not hidden.
    pub unmeasured: Vec<Unmeasured>,
    /// Left out, with the reason.
    pub excluded: Vec<Excluded>,
}

/// Nothing satisfied the need. Carries what was excluded and why, so the caller can see which
/// requirement ruled the field out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoneEligible {
    /// Everything that was considered.
    pub excluded: Vec<Excluded>,
    /// Eligible backends that had no measurement here (empty if none were eligible).
    pub unmeasured: Vec<Unmeasured>,
}

impl fmt::Display for NoneEligible {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "no backend satisfies the need: {} excluded, {} eligible but not measured in this environment",
            self.excluded.len(),
            self.unmeasured.len()
        )
    }
}

impl std::error::Error for NoneEligible {}

/// Order the candidates for `need` using `speed`. Pure: it reads the evidence it is given and
/// invents none.
///
/// A candidate is excluded if it failed a needed leaf, or its declared durability is below the
/// minimum (or undeclared when the minimum is above `Volatile`). The rest are ranked by the median
/// of `need.rank_by` measured in `need.environment`. An eligible candidate with no such measurement
/// is listed as unmeasured. If nothing can be ranked the result is an error (carrying the
/// exclusions and the unmeasured): a caller that got `Ok` has a first entry it can use.
pub fn choose(
    candidates: &[Candidate],
    need: &Need,
    speed: &SpeedTable,
    declared: &dyn Fn(&str) -> Option<Declared>,
) -> Result<Choice, NoneEligible> {
    let mut ranked = Vec::new();
    let mut unmeasured = Vec::new();
    let mut excluded = Vec::new();
    for c in candidates {
        if let Eligibility::Excluded(ex) = &c.eligibility {
            excluded.push(Excluded {
                backend: c.backend.clone(),
                version: c.version.clone(),
                why: Why::Contract(ex.clone()),
            });
            continue;
        }
        let dur = declared(&c.backend);
        match dur {
            Some(d) if d.class < need.min_durability => {
                excluded.push(Excluded {
                    backend: c.backend.clone(),
                    version: c.version.clone(),
                    why: Why::Durability {
                        declared: d.class,
                        wanted: need.min_durability,
                    },
                });
                continue;
            }
            None if need.min_durability > Durability::Volatile => {
                excluded.push(Excluded {
                    backend: c.backend.clone(),
                    version: c.version.clone(),
                    why: Why::DurabilityUnknown {
                        wanted: need.min_durability,
                    },
                });
                continue;
            }
            _ => {}
        }
        let row = speed.rows().iter().find(|r| {
            r.workload == need.rank_by
                && r.backend == c.backend
                && r.version == c.version
                && r.config == c.config
                && r.env.comparable_key() == need.environment
                && matches!(r.outcome, Outcome::Measured(_))
        });
        match row {
            Some(r) => {
                let Outcome::Measured(stats) = r.outcome else {
                    continue;
                };
                ranked.push(Entry {
                    backend: c.backend.clone(),
                    version: c.version.clone(),
                    config: c.config.clone(),
                    stats,
                    date: r.env.date.clone(),
                    digest: r.env.digest.clone(),
                    rerun: r.env.rerun.clone(),
                    durability: dur,
                });
            }
            None => unmeasured.push(Unmeasured {
                backend: c.backend.clone(),
                version: c.version.clone(),
            }),
        }
    }
    ranked.sort_by(|a, b| {
        (a.stats.p50_ns, &a.backend, &a.version).cmp(&(b.stats.p50_ns, &b.backend, &b.version))
    });
    if ranked.is_empty() {
        return Err(NoneEligible {
            excluded,
            unmeasured,
        });
    }
    Ok(Choice {
        need: need.clone(),
        ranked,
        unmeasured,
        excluded,
    })
}

impl Choice {
    /// The backend ranked first. A convenience over `ranked[0]`; the whole list is the answer.
    pub fn first(&self) -> &Entry {
        &self.ranked[0]
    }

    /// The decision as plain lines for the caller's log: the need, then each ranked backend with its
    /// evidence, then the unmeasured and the excluded with their reasons.
    pub fn log_lines(&self) -> Vec<String> {
        let n = &self.need;
        let leaves: Vec<_> = n.leaves.iter().map(String::as_str).collect();
        let mut out = vec![format!(
            "choose: need leaves=[{}] min_durability={} rank_by={} environment=[{}]",
            leaves.join(","),
            n.min_durability.as_str(),
            n.rank_by,
            n.environment
        )];
        for (i, e) in self.ranked.iter().enumerate() {
            out.push(format!(
                "choose: #{} {} {} {}  p50={}ns p95={}ns ops/s={}  measured {}{}  durability={}  re-run: {}",
                i + 1,
                e.backend,
                e.version,
                e.config,
                e.stats.p50_ns,
                e.stats.p95_ns,
                e.stats.ops_per_sec,
                e.date,
                e.digest
                    .as_ref()
                    .map(|d| format!(" image {d}"))
                    .unwrap_or_default(),
                e.durability.map_or_else(
                    || "undeclared".to_string(),
                    |d| format!(
                        "{} (declared: {}; verified: {})",
                        d.class.as_str(),
                        d.basis,
                        d.verified.map_or_else(
                            || "no".to_string(),
                            |v| format!(
                                "up to {} by {}",
                                v.as_str(),
                                if v >= Durability::Machine {
                                    "kv.durability.machine"
                                } else {
                                    "kv.durability.process"
                                }
                            )
                        )
                    )
                ),
                e.rerun
            ));
        }
        for u in &self.unmeasured {
            out.push(format!(
                "choose: eligible, not ranked (no measurement of {} here): {} {}",
                n.rank_by, u.backend, u.version
            ));
        }
        for x in &self.excluded {
            out.push(format!(
                "choose: excluded {} {}: {}",
                x.backend, x.version, x.why
            ));
        }
        out
    }
}

impl fmt::Display for Why {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Why::Contract(ex) => {
                let parts: Vec<_> = ex
                    .iter()
                    .map(|e| format!("{} {}", e.leaf, e.reason.as_str()))
                    .collect();
                write!(f, "failed the contract ({})", parts.join(", "))
            }
            Why::Durability { declared, wanted } => write!(
                f,
                "declares durability {} but {} is required",
                declared.as_str(),
                wanted.as_str()
            ),
            Why::DurabilityUnknown { wanted } => write!(
                f,
                "no durability declaration and {} is required",
                wanted.as_str()
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bench::{Environment, SpeedRow};

    fn env(machine: &str) -> Environment {
        Environment {
            machine: machine.to_string(),
            os: "linux".to_string(),
            arch: "x86_64".to_string(),
            date: "2026-10-16".to_string(),
            digest: None,
            rerun: "re-run".to_string(),
            host: None,
        }
    }

    fn row(backend: &str, machine: &str, p50: u64) -> SpeedRow {
        SpeedRow {
            workload: "speed.x.get".to_string(),
            backend: backend.to_string(),
            version: "1".to_string(),
            config: Config::new(),
            env: env(machine),
            outcome: Outcome::Measured(Stats::from_samples(vec![p50; 10]).unwrap()),
        }
    }

    fn cand(backend: &str, ok: bool) -> Candidate {
        Candidate {
            backend: backend.to_string(),
            version: "1".to_string(),
            config: Config::new(),
            eligibility: if ok {
                Eligibility::Eligible
            } else {
                Eligibility::Excluded(vec![LeafFailure {
                    leaf: "x.get".to_string(),
                    reason: Reason::Wrong,
                    note: "lost a write".to_string(),
                }])
            },
        }
    }

    fn need(min: Durability) -> Need {
        Need {
            leaves: vec!["x.get".to_string()],
            min_durability: min,
            rank_by: "speed.x.get".to_string(),
            environment: env("laptop").comparable_key(),
        }
    }

    fn table(b: &str) -> Option<Declared> {
        let class = match b {
            "file" => Durability::Process,
            "server" => Durability::Machine,
            "memory" => Durability::Volatile,
            _ => return None,
        };
        Some(Declared {
            class,
            basis: "test table",
            verified: (class >= Durability::Process).then_some(Durability::Process),
        })
    }

    #[test]
    fn a_machine_verified_backend_names_the_machine_test_in_the_log() {
        fn machine(b: &str) -> Option<Declared> {
            let mut d = table(b)?;
            d.verified = Some(Durability::Machine);
            Some(d)
        }
        let speed = SpeedTable::from_rows(vec![row("server", "laptop", 2000)]);
        let c = choose(
            &[cand("server", true)],
            &need(Durability::Machine),
            &speed,
            &machine,
        )
        .unwrap();
        let log = c.log_lines().join("\n");
        assert!(
            log.contains("verified: up to machine by kv.durability.machine"),
            "{log}"
        );
        // The control: a Process verification still names the process test.
        let c = choose(
            &[cand("file", true)],
            &need(Durability::Process),
            &SpeedTable::from_rows(vec![row("file", "laptop", 300)]),
            &table,
        )
        .unwrap();
        assert!(
            c.log_lines()
                .join("\n")
                .contains("by kv.durability.process")
        );
    }

    #[test]
    fn the_fastest_eligible_backend_is_first_and_carries_its_evidence() {
        let speed = SpeedTable::from_rows(vec![
            row("file", "laptop", 300),
            row("server", "laptop", 2000),
        ]);
        let cands = [cand("server", true), cand("file", true)];
        let c = choose(&cands, &need(Durability::Process), &speed, &table).unwrap();
        assert_eq!(c.first().backend, "file");
        assert_eq!(c.first().stats.p50_ns, 300);
        assert_eq!(c.first().rerun, "re-run");
        let log = c.log_lines().join("\n");
        assert!(
            log.contains("min_durability=process") && log.contains("re-run: re-run"),
            "{log}"
        );
        assert!(log.contains("verified: up to process"), "{log}");
    }

    #[test]
    fn a_minimum_durability_excludes_a_faster_backend_and_says_so() {
        let speed = SpeedTable::from_rows(vec![
            row("file", "laptop", 300),
            row("server", "laptop", 2000),
        ]);
        let cands = [cand("file", true), cand("server", true)];
        let c = choose(&cands, &need(Durability::Machine), &speed, &table).unwrap();
        assert_eq!(c.ranked.len(), 1);
        assert_eq!(c.first().backend, "server");
        assert!(matches!(
            c.excluded[0].why,
            Why::Durability {
                declared: Durability::Process,
                wanted: Durability::Machine
            }
        ));
        assert!(
            c.log_lines()
                .join("\n")
                .contains("declares durability process but machine is required")
        );
    }

    #[test]
    fn an_undeclared_backend_is_unknown_not_volatile() {
        let speed = SpeedTable::from_rows(vec![row("mystery", "laptop", 1)]);
        let one = [cand("mystery", true)];
        let c = choose(&one, &need(Durability::Volatile), &speed, &table).unwrap();
        assert_eq!(
            c.first().backend,
            "mystery",
            "volatile needs no declaration"
        );
        let e = choose(&one, &need(Durability::Process), &speed, &table).unwrap_err();
        assert!(matches!(e.excluded[0].why, Why::DurabilityUnknown { .. }));
    }

    #[test]
    fn a_contract_failure_is_excluded_however_fast() {
        let speed =
            SpeedTable::from_rows(vec![row("memory", "laptop", 1), row("file", "laptop", 300)]);
        let cands = [cand("memory", false), cand("file", true)];
        let c = choose(&cands, &need(Durability::Volatile), &speed, &table).unwrap();
        assert_eq!(c.first().backend, "file");
        assert!(matches!(&c.excluded[0].why, Why::Contract(ex) if ex[0].reason == Reason::Wrong));
    }

    #[test]
    fn measurements_from_another_machine_are_not_used() {
        let speed = SpeedTable::from_rows(vec![row("memory", "server", 1)]);
        let one = [cand("memory", true)];
        let e = choose(&one, &need(Durability::Volatile), &speed, &table).unwrap_err();
        assert!(e.excluded.is_empty());
        assert_eq!(
            e.unmeasured.len(),
            1,
            "eligible, but nothing measured on this machine"
        );
    }

    #[test]
    fn nothing_eligible_is_an_error_with_the_reasons_not_a_fallback() {
        let speed = SpeedTable::from_rows(vec![row("memory", "laptop", 1)]);
        let one = [cand("memory", false)];
        let e = choose(&one, &need(Durability::Volatile), &speed, &table).unwrap_err();
        assert_eq!(e.excluded.len(), 1);
        assert!(e.to_string().contains("no backend satisfies"));
    }
}
