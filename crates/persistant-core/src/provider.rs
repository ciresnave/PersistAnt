// SPDX-License-Identifier: MIT OR Apache-2.0
//! **"Which backends can give me all of this?"** The consumer lists the capabilities it needs
//! ([`ProviderNeeds`]); [`providers`] returns every [`Provider`] in a table whose recorded matrix rows
//! say yes to **every** one, and whose declared durability reaches the minimum. [`rank`] orders those
//! with [`choose`](crate::choose::choose). Nothing here opens a backend: opening is the facade's
//! `registry` (a provider says what endpoint it needs, [`EndpointSpec`], and nothing more).
//!
//! The rules are `docs/DESIGN-KINDS.md` section 2.1's, applied to a field of providers instead of one
//! named backend:
//!
//! - A capability is a boolean. A provider has it only if the [`Matrix`] says [`Lookup::On`] on one of
//!   its [`Surface`]s and [`Lookup::Off`] on none. No row is off.
//! - **No closest match.** If no provider satisfies every need, the answer is [`Unmet`]: for each
//!   provider, each needed id it lacks (with the matrix's own word, [`Lookup`]) and any durability
//!   shortfall. A partial list is never returned.
//! - The table is data the caller passes in; this module holds no registry and does no I/O.
//!
//! ```
//! use persistant_core::{CapabilityId, Config, Matrix};
//! use persistant_core::choose::Durability;
//! use persistant_core::provider::{providers, EndpointSpec, Provider, ProviderNeeds, Surface};
//!
//! let matrix = Matrix::parse("subject_kind\tsubject\tbackend\tversion\tconfig\tenabled\treason\tmetadata\tnote\n\
//!     capability\tkv.get\tmem\t1\t\tyes\t\t\t\n").unwrap();
//! let table = [Provider::new("mem", vec![Surface::new("mem", "1", Config::new())], None, EndpointSpec::None)];
//! let needs = ProviderNeeds::new(vec![CapabilityId::new("kv.get").unwrap()], Durability::Volatile);
//! assert_eq!(providers(&table, &needs, &matrix).unwrap().len(), 1);
//! let needs = ProviderNeeds::new(vec![CapabilityId::new("kv.ttl").unwrap()], Durability::Volatile);
//! assert!(providers(&table, &needs, &matrix).is_err()); // names `kv.ttl` as unmet on `mem`
//! ```
//!
//! From `providers()` to a working handle (the open half lands with the facade's `registry`):
//! 1. build a [`ProviderNeeds`] from the capability ids and the lowest durability you accept;
//! 2. call [`providers`] with the table and the matrix; `Err(Unmet)` names what each backend lacks;
//! 3. optionally [`rank`] the result by a measured workload;
//! 4. take the first [`Provider`]; its [`EndpointSpec`] says whether it needs a path, a URL or nothing;
//! 5. pass it and your endpoint to the registry's `open` for a handle.

use crate::choose::{Candidate, Declared, Durability, Eligibility, Need, NoneEligible};
use crate::{CapabilityId, Config, Lookup, Matrix};
use std::fmt;

/// What a provider needs to be told to open: the endpoint information a configuration cannot guess.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum EndpointSpec {
    /// Nothing (in-process memory).
    None,
    /// A directory or file on the local filesystem.
    Path,
    /// A connection URL whose scheme is one of these (`postgres`, `mysql`, `sqlite`).
    Url {
        /// The accepted URL schemes.
        schemes: Vec<String>,
    },
}

/// One place the matrix records facts about a provider: a `(backend, version, config)` triple. A
/// provider that is a kind built on another (key-value on PostgreSQL) has one surface per layer,
/// because the matrix files name them separately (`kv-on-relational:postgres` and `postgres`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Surface {
    /// Backend name as in the matrix.
    pub backend: String,
    /// Backend version as in the matrix.
    pub version: String,
    /// The configuration facts the rows are matched against.
    pub config: Config,
}

impl Surface {
    /// A surface for `backend` at `version` with `config`.
    pub fn new(backend: impl Into<String>, version: impl Into<String>, config: Config) -> Self {
        Surface {
            backend: backend.into(),
            version: version.into(),
            config,
        }
    }
}

/// A backend a program could be given: what the matrix is asked about, what it declares about
/// durability, and what endpoint it needs to be opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Provider {
    name: String,
    surfaces: Vec<Surface>,
    declared: Option<Declared>,
    endpoint: EndpointSpec,
}

impl Provider {
    /// A provider called `name` (unique within its table). `surfaces` must not be empty: the first
    /// is the **primary** one, the layer a program talks to, which [`rank`] matches speed rows on.
    ///
    /// # Panics
    /// If `surfaces` is empty: a provider with nothing to look up could satisfy anything.
    pub fn new(
        name: impl Into<String>,
        surfaces: Vec<Surface>,
        declared: Option<Declared>,
        endpoint: EndpointSpec,
    ) -> Self {
        assert!(
            !surfaces.is_empty(),
            "a Provider needs at least one Surface"
        );
        Provider {
            name: name.into(),
            surfaces,
            declared,
            endpoint,
        }
    }

    /// The provider's name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Where the matrix is asked, primary first.
    pub fn surfaces(&self) -> &[Surface] {
        &self.surfaces
    }

    /// The primary surface.
    pub fn primary(&self) -> &Surface {
        &self.surfaces[0]
    }

    /// Declared durability, if the provider's table has one. `None` is unknown, not volatile.
    pub fn declared(&self) -> Option<Declared> {
        self.declared
    }

    /// What it needs to open.
    pub fn endpoint(&self) -> &EndpointSpec {
        &self.endpoint
    }

    fn lookup(&self, matrix: &Matrix, id: &CapabilityId) -> Lookup {
        let mut on = false;
        let mut off = None;
        for s in &self.surfaces {
            match matrix.lookup(id, &s.backend, &s.version, &s.config) {
                Lookup::On => on = true,
                Lookup::Off(r) => {
                    off.get_or_insert(r);
                }
                _ => {}
            }
        }
        match (on, off) {
            (_, Some(r)) => Lookup::Off(r),
            (true, None) => Lookup::On,
            (false, None) => Lookup::NoRow,
        }
    }
}

/// What the program needs: capability ids that must all be on, and the lowest durability class.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderNeeds {
    ids: Vec<CapabilityId>,
    min_durability: Durability,
    preferred: Vec<CapabilityId>,
}

impl ProviderNeeds {
    /// Every id in `ids` must be on, and durability must reach `min_durability`.
    pub fn new(ids: Vec<CapabilityId>, min_durability: Durability) -> Self {
        ProviderNeeds {
            ids,
            min_durability,
            preferred: Vec::new(),
        }
    }

    /// What matters most, **most important first**. A preferred id never excludes a provider; it
    /// only orders the ones that qualify ([`sorted`]).
    #[must_use]
    pub fn prefer(mut self, preferred: Vec<CapabilityId>) -> Self {
        self.preferred = preferred;
        self
    }

    /// The preferred ids, most important first.
    pub fn preferred(&self) -> &[CapabilityId] {
        &self.preferred
    }

    /// The needed ids.
    pub fn ids(&self) -> &[CapabilityId] {
        &self.ids
    }

    /// The lowest acceptable durability.
    pub fn min_durability(&self) -> Durability {
        self.min_durability
    }
}

/// Why a provider's durability fell short.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DurabilityShortfall {
    /// It declares a lower class.
    Below(Durability),
    /// It declares nothing, and more than `Volatile` was asked for.
    Unknown,
}

/// What one provider lacks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderUnmet {
    /// The provider's name.
    pub provider: String,
    /// Each needed id it does not have, with what the matrix said ([`Lookup::Off`] or
    /// [`Lookup::NoRow`]). Never contains [`Lookup::On`].
    pub missing: Vec<(CapabilityId, Lookup)>,
    /// Set if its durability is below the minimum.
    pub durability: Option<DurabilityShortfall>,
}

/// No provider satisfies the whole need. Lists **every** provider considered with what it lacks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unmet {
    /// One entry per provider in the table; an empty table gives an empty list.
    pub providers: Vec<ProviderUnmet>,
}

impl fmt::Display for Unmet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.providers.is_empty() {
            return f.write_str("no provider is registered");
        }
        f.write_str("no provider satisfies the need:")?;
        for p in &self.providers {
            write!(f, " [{}:", p.provider)?;
            for (id, l) in &p.missing {
                match l {
                    Lookup::Off(r) => write!(f, " {} off({r})", id.as_str())?,
                    _ => write!(f, " {} no-row", id.as_str())?,
                }
            }
            match p.durability {
                Some(DurabilityShortfall::Below(d)) => write!(f, " durability {}", d.as_str())?,
                Some(DurabilityShortfall::Unknown) => f.write_str(" durability unknown")?,
                None => {}
            }
            f.write_str("]")?;
        }
        Ok(())
    }
}

impl std::error::Error for Unmet {}

/// Every provider in `table` that has all of `needs`, in table order.
///
/// A provider qualifies if each needed id is [`Lookup::On`] on at least one of its surfaces and
/// [`Lookup::Off`] on none, and its declared durability is at least `needs.min_durability()`
/// (undeclared fails unless `Volatile` is enough). If none qualifies the error names, for every
/// provider, what it lacks. No closest match is offered.
pub fn providers(
    table: &[Provider],
    needs: &ProviderNeeds,
    matrix: &Matrix,
) -> Result<Vec<Provider>, Unmet> {
    let mut ok = Vec::new();
    let mut unmet = Vec::new();
    for p in table {
        let missing: Vec<_> = needs
            .ids
            .iter()
            .filter_map(|id| match p.lookup(matrix, id) {
                Lookup::On => None,
                other => Some((id.clone(), other)),
            })
            .collect();
        let durability = match p.declared {
            Some(d) if d.class < needs.min_durability => Some(DurabilityShortfall::Below(d.class)),
            None if needs.min_durability > Durability::Volatile => {
                Some(DurabilityShortfall::Unknown)
            }
            _ => None,
        };
        if missing.is_empty() && durability.is_none() {
            ok.push(p.clone());
        } else {
            unmet.push(ProviderUnmet {
                provider: p.name.clone(),
                missing,
                durability,
            });
        }
    }
    if ok.is_empty() {
        return Err(Unmet { providers: unmet });
    }
    Ok(ok)
}

/// Order `qualified` (the result of [`providers`]) with [`choose`](crate::choose::choose): fastest
/// median first for `need.rank_by` in `need.environment`, matched on each provider's primary surface.
/// Eligibility comes from the recorded rows already checked, so `need.leaves` is not consulted. A
/// provider with no measurement is listed as unmeasured, not hidden; if none is measured the result is
/// the error that carries them.
pub fn rank(
    qualified: &[Provider],
    need: &Need,
    speed: &crate::bench::SpeedTable,
) -> Result<crate::choose::Choice, NoneEligible> {
    let candidates: Vec<_> = qualified
        .iter()
        .map(|p| Candidate {
            backend: p.primary().backend.clone(),
            version: p.primary().version.clone(),
            config: p.primary().config.clone(),
            eligibility: Eligibility::Eligible,
        })
        .collect();
    let declared = |backend: &str| {
        qualified
            .iter()
            .find(|p| p.primary().backend == backend)
            .and_then(|p| p.declared)
    };
    crate::choose::choose(&candidates, need, speed, &declared)
}

/// One provider in a sorted answer, with what it was sorted on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sorted {
    /// The provider.
    pub provider: Provider,
    /// Each preferred id, in the order of importance given, and whether this provider has it.
    pub preferred: Vec<(CapabilityId, bool)>,
    /// Median time of the ranking workload in the ranking environment, if measured.
    pub p50_ns: Option<u64>,
}

/// Order `qualified` (the result of [`providers`]) for a setup UI: by the preferred ids in order of
/// importance (a provider having the first preferred id outranks any that lacks it, whatever the
/// rest), then by measured median time, smaller first, with unmeasured last, then by name. Nothing is
/// dropped: every provider passed in is in the result, with the evidence it was sorted on. `speed`
/// is the [`Need`] naming the workload and environment, and the table; without it nothing is measured.
pub fn sorted(
    qualified: &[Provider],
    needs: &ProviderNeeds,
    matrix: &Matrix,
    speed: Option<(&Need, &crate::bench::SpeedTable)>,
) -> Vec<Sorted> {
    use crate::bench::Outcome;
    let mut out: Vec<Sorted> = qualified
        .iter()
        .map(|p| {
            let p50_ns = speed.and_then(|(need, table)| {
                let s = p.primary();
                table.rows().iter().find_map(|r| match r.outcome {
                    Outcome::Measured(st)
                        if r.workload == need.rank_by
                            && r.backend == s.backend
                            && r.version == s.version
                            && r.config == s.config
                            && r.env.comparable_key() == need.environment =>
                    {
                        Some(st.p50_ns)
                    }
                    _ => None,
                })
            });
            Sorted {
                provider: p.clone(),
                preferred: needs
                    .preferred
                    .iter()
                    .map(|id| (id.clone(), p.lookup(matrix, id) == Lookup::On))
                    .collect(),
                p50_ns,
            }
        })
        .collect();
    out.sort_by(|a, b| {
        let met = |x: &Sorted| x.preferred.iter().map(|(_, m)| !*m).collect::<Vec<_>>();
        met(a)
            .cmp(&met(b))
            .then_with(|| match (a.p50_ns, b.p50_ns) {
                (Some(x), Some(y)) => x.cmp(&y),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => std::cmp::Ordering::Equal,
            })
            .then_with(|| a.provider.name.cmp(&b.provider.name))
    });
    out
}

/// [`providers`] then [`sorted`]: the one call a setup UI makes.
pub fn select(
    table: &[Provider],
    needs: &ProviderNeeds,
    matrix: &Matrix,
    speed: Option<(&Need, &crate::bench::SpeedTable)>,
) -> Result<Vec<Sorted>, Unmet> {
    let q = providers(table, needs, matrix)?;
    Ok(sorted(&q, needs, matrix, speed))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Reason;

    const HEADER: &str =
        "subject_kind\tsubject\tbackend\tversion\tconfig\tenabled\treason\tmetadata\tnote\n";

    fn row(id: &str, backend: &str, yes: bool) -> String {
        let (e, r) = if yes { ("yes", "") } else { ("no", "wrong") };
        format!("capability\t{id}\t{backend}\t1\t\t{e}\t{r}\t\t\n")
    }

    fn matrix(rows: &[(&str, &str, bool)]) -> Matrix {
        let mut s = HEADER.to_string();
        for (i, b, y) in rows {
            s.push_str(&row(i, b, *y));
        }
        Matrix::parse(&s).unwrap()
    }

    fn id(s: &str) -> CapabilityId {
        CapabilityId::new(s).unwrap()
    }

    fn decl(class: Durability) -> Option<Declared> {
        Some(Declared {
            class,
            basis: "test",
            verified: None,
        })
    }

    fn prov(name: &str, backends: &[&str], d: Option<Declared>) -> Provider {
        let s = backends
            .iter()
            .map(|b| Surface::new(*b, "1", Config::new()))
            .collect();
        Provider::new(name, s, d, EndpointSpec::None)
    }

    fn needs(ids: &[&str], d: Durability) -> ProviderNeeds {
        ProviderNeeds::new(ids.iter().map(|i| id(i)).collect(), d)
    }

    #[test]
    fn every_id_must_be_on_and_a_known_bad_flips_the_provider_out() {
        let table = [
            prov("a", &["a"], decl(Durability::Machine)),
            prov("b", &["b"], decl(Durability::Machine)),
        ];
        let m = matrix(&[
            ("kv.get", "a", true),
            ("kv.ttl", "a", true),
            ("kv.get", "b", true),
            ("kv.ttl", "b", true),
        ]);
        let n = needs(&["kv.get", "kv.ttl"], Durability::Process);
        let got = providers(&table, &n, &m).unwrap();
        assert_eq!(
            got.iter().map(Provider::name).collect::<Vec<_>>(),
            ["a", "b"]
        );
        // Known-bad: the same need, one id Off on `b`: `b` is out and `a` stays.
        let m = matrix(&[
            ("kv.get", "a", true),
            ("kv.ttl", "a", true),
            ("kv.get", "b", true),
            ("kv.ttl", "b", false),
        ]);
        let got = providers(&table, &n, &m).unwrap();
        assert_eq!(got.iter().map(Provider::name).collect::<Vec<_>>(), ["a"]);
    }

    #[test]
    fn nothing_satisfying_names_each_providers_unmet_id_and_offers_no_partial() {
        let table = [
            prov("a", &["a"], decl(Durability::Machine)),
            prov("b", &["b"], decl(Durability::Machine)),
        ];
        // a has kv.get only; b has kv.ttl only (wrong for get). Neither has both.
        let m = matrix(&[
            ("kv.get", "a", true),
            ("kv.ttl", "b", true),
            ("kv.get", "b", false),
        ]);
        let e = providers(
            &table,
            &needs(&["kv.get", "kv.ttl"], Durability::Volatile),
            &m,
        )
        .unwrap_err();
        assert_eq!(e.providers.len(), 2);
        assert_eq!(e.providers[0].provider, "a");
        assert_eq!(e.providers[0].missing, vec![(id("kv.ttl"), Lookup::NoRow)]);
        assert_eq!(e.providers[1].provider, "b");
        assert_eq!(
            e.providers[1].missing,
            vec![(id("kv.get"), Lookup::Off(Reason::Wrong))]
        );
        let text = e.to_string();
        assert!(text.contains("[a: kv.ttl no-row]"), "{text}");
        assert!(text.contains("[b: kv.get off(wrong)]"), "{text}");
    }

    #[test]
    fn durability_is_a_boolean_gate_and_unknown_fails_above_volatile() {
        let table = [
            prov("vol", &["v"], decl(Durability::Volatile)),
            prov("unk", &["u"], None),
        ];
        let m = matrix(&[("kv.get", "v", true), ("kv.get", "u", true)]);
        let e = providers(&table, &needs(&["kv.get"], Durability::Process), &m).unwrap_err();
        assert_eq!(
            e.providers[0].durability,
            Some(DurabilityShortfall::Below(Durability::Volatile))
        );
        assert_eq!(
            e.providers[1].durability,
            Some(DurabilityShortfall::Unknown)
        );
        assert!(e.providers.iter().all(|p| p.missing.is_empty()));
        // Positive control: Volatile is enough, so both qualify.
        let got = providers(&table, &needs(&["kv.get"], Durability::Volatile), &m).unwrap();
        assert_eq!(got.len(), 2);
    }

    #[test]
    fn surfaces_combine_across_layers_and_off_on_any_layer_wins() {
        let table = [prov("pg", &["kv-on-pg", "pg"], decl(Durability::Machine))];
        let m = matrix(&[("kv.get", "kv-on-pg", true), ("rel.tx", "pg", true)]);
        let n = needs(&["kv.get", "rel.tx"], Durability::Machine);
        assert_eq!(providers(&table, &n, &m).unwrap().len(), 1);
        let m = matrix(&[
            ("kv.get", "kv-on-pg", true),
            ("rel.tx", "pg", true),
            ("rel.tx", "kv-on-pg", false),
        ]);
        let e = providers(&table, &n, &m).unwrap_err();
        assert_eq!(
            e.providers[0].missing,
            vec![(id("rel.tx"), Lookup::Off(Reason::Wrong))]
        );
    }

    #[test]
    fn an_empty_table_is_an_error_and_an_empty_need_takes_everything() {
        let m = matrix(&[]);
        let e = providers(&[], &needs(&[], Durability::Volatile), &m).unwrap_err();
        assert!(e.providers.is_empty());
        assert_eq!(e.to_string(), "no provider is registered");
        let table = [prov("a", &["a"], decl(Durability::Volatile))];
        assert_eq!(
            providers(&table, &needs(&[], Durability::Volatile), &m)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn rank_orders_by_the_speed_table_through_choose() {
        use crate::bench::{Environment, Outcome, SpeedRow, SpeedTable, Stats};
        let env = Environment {
            machine: "m".into(),
            os: "linux".into(),
            arch: "x86_64".into(),
            date: "2026-10-21".into(),
            digest: None,
            rerun: "cmd".into(),
        };
        let stats = |p50| Stats {
            n: 5,
            p50_ns: p50,
            p95_ns: p50,
            p99_ns: p50,
            max_ns: p50,
            ops_per_sec: 1,
        };
        let mk = |b: &str, p50| SpeedRow {
            workload: "w".into(),
            backend: b.into(),
            version: "1".into(),
            config: Config::new(),
            env: env.clone(),
            outcome: Outcome::Measured(stats(p50)),
        };
        let speed = SpeedTable::from_rows(vec![mk("slow", 90), mk("fast", 10)]);
        let q = [
            prov("slow", &["slow"], decl(Durability::Volatile)),
            prov("fast", &["fast"], decl(Durability::Volatile)),
            prov("unmeasured", &["nope"], decl(Durability::Volatile)),
        ];
        let need = Need {
            leaves: vec![],
            min_durability: Durability::Volatile,
            rank_by: "w".into(),
            environment: env.comparable_key(),
        };
        let c = rank(&q, &need, &speed).unwrap();
        let names: Vec<_> = c.ranked.iter().map(|e| e.backend.as_str()).collect();
        assert_eq!(names, ["fast", "slow"]);
        assert_eq!(c.unmeasured.len(), 1);
        assert_eq!(c.unmeasured[0].backend, "nope");
    }

    #[test]
    fn first_preferred_outweighs_all_later_ones_then_speed_then_name() {
        use crate::bench::{Environment, Outcome, SpeedRow, SpeedTable, Stats};
        let d = decl(Durability::Volatile);
        let table = [
            prov("a", &["a"], d),
            prov("b", &["b"], d),
            prov("c", &["c"], d),
            prov("d", &["d"], d),
        ];
        // All have kv.get (required). a: p2 only. b: p1 only. c: p1 and p2. d: neither.
        let m = matrix(&[
            ("kv.get", "a", true),
            ("kv.get", "b", true),
            ("kv.get", "c", true),
            ("kv.get", "d", true),
            ("kv.ttl", "a", true),
            ("kv.ttl", "b", true),
            ("kv.ttl", "c", true),
            ("kv.cas", "c", true),
            ("kv.cas", "a", true),
        ]);
        // Importance: kv.ttl first, kv.cas second.
        let n = needs(&["kv.get"], Durability::Volatile).prefer(vec![id("kv.ttl"), id("kv.cas")]);
        let got = select(&table, &n, &m, None).unwrap();
        let names: Vec<_> = got.iter().map(|s| s.provider.name()).collect();
        // a and c have both preferred ids and tie, so name order: a, c; then b (first only); then d.
        assert_eq!(names, ["a", "c", "b", "d"]);
        // Evidence is carried.
        assert_eq!(
            got[2].preferred,
            vec![(id("kv.ttl"), true), (id("kv.cas"), false)]
        );

        // Speed breaks a tie, unmeasured last, and a preferred id never excludes.
        let env = Environment {
            machine: "m".into(),
            os: "linux".into(),
            arch: "x86_64".into(),
            date: "2026-10-21".into(),
            digest: None,
            rerun: "cmd".into(),
        };
        let st = |p50| Stats {
            n: 5,
            p50_ns: p50,
            p95_ns: p50,
            p99_ns: p50,
            max_ns: p50,
            ops_per_sec: 1,
        };
        let mk = |b: &str, p50| SpeedRow {
            workload: "w".into(),
            backend: b.into(),
            version: "1".into(),
            config: Config::new(),
            env: env.clone(),
            outcome: Outcome::Measured(st(p50)),
        };
        let speed = SpeedTable::from_rows(vec![mk("a", 90), mk("c", 10)]);
        let need = Need {
            leaves: vec![],
            min_durability: Durability::Volatile,
            rank_by: "w".into(),
            environment: env.comparable_key(),
        };
        let got = select(&table, &n, &m, Some((&need, &speed))).unwrap();
        let names: Vec<_> = got.iter().map(|s| s.provider.name()).collect();
        assert_eq!(names, ["c", "a", "b", "d"]);
        assert_eq!(got[0].p50_ns, Some(10));
        assert_eq!(got[3].p50_ns, None);
    }

    #[test]
    fn select_is_an_error_when_the_required_set_is_unmet_whatever_is_preferred() {
        let table = [prov("a", &["a"], decl(Durability::Volatile))];
        let m = matrix(&[("kv.ttl", "a", true)]);
        let n = needs(&["kv.get"], Durability::Volatile).prefer(vec![id("kv.ttl")]);
        assert!(select(&table, &n, &m, None).is_err());
    }
}
