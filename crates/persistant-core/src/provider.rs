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
//! - Selection is `capant`'s (`capant::evaluate`), asked about this crate's rows through
//!   [`Matrix::to_capant`]. A provider has a capability only if a row says yes **with its probe id**
//!   on one of its [`Surface`]s and none says no. A row without evidence, and no row at all, are
//!   *unverified*, not off: the unmet text says `unverified(<why>)`.
//! - The minimum durability is a capability too (`durability.process`, `durability.machine`): a
//!   provider that declares a class has a row for it and for every lower class, with the declared
//!   basis as its evidence. `Volatile` is asked for nothing, so a provider that declares nothing
//!   still qualifies for it.
//! - **No closest match.** If no provider satisfies every need, the answer is [`Unmet`]: for each
//!   provider, each needed id it lacks (with the matrix's own word, [`Lookup`], and for an
//!   unverified one the reason) and any durability shortfall. A partial list is never returned.
//! - The table is data the caller passes in; this module holds no registry and does no I/O.
//!
//! ```
//! use persistant_core::{CapabilityId, Config, Matrix};
//! use persistant_core::choose::Durability;
//! use persistant_core::provider::{providers, EndpointSpec, Provider, ProviderNeeds, Surface};
//!
//! let matrix = Matrix::parse("subject_kind\tsubject\tbackend\tversion\tconfig\tenabled\treason\tmetadata\tnote\n\
//!     capability\tkv.get\tmem\t1\t\tyes\t\tprobe=kv::kv.get\t\n").unwrap();
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
use crate::{CapabilityId, Config, Lookup, Matrix, Reason};
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
    /// For each id in `missing` that is [`Lookup::NoRow`], why it is unverified (`no row`,
    /// `no evidence`, `stale`, ...), in the words of `capant`. Empty for an id that is off.
    pub unverified: Vec<(CapabilityId, String)>,
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
                    _ => {
                        let why = p
                            .unverified
                            .iter()
                            .find(|(i, _)| i == id)
                            .map_or("no row", |(_, w)| w.as_str());
                        write!(f, " {} unverified({why})", id.as_str())?
                    }
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

/// The id of the durability capability for `class`: what a provider that declares it has, and what a
/// need for at least that class requires.
fn durability_id(class: Durability) -> CapabilityId {
    CapabilityId::new(format!("durability.{}", class.as_str()))
        .expect("`durability.` and a lower-case word is inside the id grammar")
}

/// The classes up to and including `class`, lowest first. A `match` with no wildcard: a class added
/// to [`Durability`] stops this compiling until it is placed.
fn classes_up_to(class: Durability) -> Vec<Durability> {
    let rank = |c: Durability| match c {
        Durability::Volatile => 0,
        Durability::Process => 1,
        Durability::Machine => 2,
    };
    [
        Durability::Volatile,
        Durability::Process,
        Durability::Machine,
    ]
    .into_iter()
    .filter(|c| rank(*c) <= rank(class))
    .collect()
}

fn cap(id: &CapabilityId) -> capant::CapabilityId {
    capant::CapabilityId::new(id.as_str()).expect("this crate's id grammar is inside capant's")
}

/// `capant`'s answer for `table` against `needs`. Each provider is given to `capant` under its
/// position in the table (two providers may share a name here; `capant` refuses that), with one
/// extra surface of its own that carries the durability rows it declares.
fn evaluate(
    table: &[Provider],
    needs: &ProviderNeeds,
    matrix: &Matrix,
) -> (capant::Evaluation, Option<CapabilityId>) {
    let durable =
        (needs.min_durability > Durability::Volatile).then(|| durability_id(needs.min_durability));
    let mut rows = matrix.to_capant().rows().to_vec();
    let cap_table: Vec<capant::Provider> = table
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let mut surfaces: Vec<capant::Surface> = p
                .surfaces
                .iter()
                .map(|s| {
                    capant::Surface::new(
                        s.backend.clone(),
                        s.version.clone(),
                        crate::adapter::config_to(&s.config, None),
                    )
                })
                .collect();
            if let Some(d) = p.declared {
                let backend = format!("declared-durability:{i}");
                for class in classes_up_to(d.class) {
                    rows.push(capant::Row {
                        subject_kind: "capability".to_owned(),
                        subject: cap(&durability_id(class)),
                        backend: backend.clone(),
                        version: String::new(),
                        config: capant::Config::new(),
                        state: capant::State::Yes,
                        evidence: capant::Evidence::probe(format!("declared: {}", d.basis)),
                        note: String::new(),
                        metadata: capant::Config::new(),
                        extra: std::collections::BTreeMap::new(),
                    });
                }
                surfaces.push(capant::Surface::new(
                    backend,
                    String::new(),
                    capant::Config::new(),
                ));
            }
            capant::Provider::new(i.to_string(), surfaces).expect("a provider has a surface")
        })
        .collect();
    let mut required: Vec<capant::CapabilityId> = needs.ids.iter().map(cap).collect();
    required.extend(durable.iter().map(cap));
    let mut need = capant::Need::new(required);
    need.preferred = needs.preferred.iter().map(cap).collect();
    let ctx = capant::Context::new(0, capant::Config::new());
    let ev = capant::evaluate(&cap_table, &need, &capant::Matrix::from_rows(rows), &ctx)
        .expect("positions are unique");
    (ev, durable)
}

/// The provider's position in the table, from the id it was given to `capant` under.
fn position(e: &capant::Entry) -> usize {
    e.provider.parse().expect("an id this module gave")
}

fn unmet_of(p: &Provider, e: &capant::Entry, durable: Option<&CapabilityId>) -> ProviderUnmet {
    use capant::{Reason as R, State};
    let mut missing = Vec::new();
    let mut unverified = Vec::new();
    let mut durability = None;
    for o in e.unmet() {
        let id = CapabilityId::new(o.id.as_str()).expect("an id this crate gave");
        if Some(&id) == durable {
            durability = Some(match p.declared {
                Some(d) => DurabilityShortfall::Below(d.class),
                None => DurabilityShortfall::Unknown,
            });
            continue;
        }
        let lookup = match &o.state {
            State::No(R::Missing) => Lookup::Off(Reason::Missing),
            State::No(R::Wrong) => Lookup::Off(Reason::Wrong),
            State::No(R::Other(t)) if t == "untested" => Lookup::Off(Reason::Untested),
            State::Unverified(w) => {
                unverified.push((id.clone(), w.to_string()));
                Lookup::NoRow
            }
            _ => Lookup::NoRow,
        };
        missing.push((id, lookup));
    }
    ProviderUnmet {
        provider: p.name.clone(),
        missing,
        unverified,
        durability,
    }
}

/// Every provider in `table` that has all of `needs`, in table order.
///
/// Asked of `capant::evaluate`: a provider qualifies if each needed id is shown to hold (a yes row
/// with its probe id) on one of its [`Surface`]s and none says no, and its declared durability is at
/// least `needs.min_durability()` (undeclared fails unless `Volatile` is enough). If none qualifies
/// the error names, for every provider, what it lacks. No closest match is offered.
pub fn providers(
    table: &[Provider],
    needs: &ProviderNeeds,
    matrix: &Matrix,
) -> Result<Vec<Provider>, Unmet> {
    let (ev, durable) = evaluate(table, needs, matrix);
    let mut ok: Vec<usize> = ev.satisfying().map(position).collect();
    if ok.is_empty() {
        let mut entries: Vec<&capant::Entry> = ev.entries().iter().collect();
        entries.sort_by_key(|e| position(e));
        return Err(Unmet {
            providers: entries
                .into_iter()
                .map(|e| unmet_of(&table[position(e)], e, durable.as_ref()))
                .collect(),
        });
    }
    ok.sort_unstable();
    Ok(ok.into_iter().map(|i| table[i].clone()).collect())
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
    let p50 = |p: &Provider| {
        speed.and_then(|(need, table)| {
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
        })
    };
    let (mut ev, _) = evaluate(qualified, needs, matrix);
    // `capant` ranks satisfiers by the preferred ids in order of importance; the measured median and
    // the name break its ties, as the preferred vector is compared again here so the order is this
    // function's own whatever `capant` does with a tie.
    let met = |e: &capant::Entry| -> Vec<bool> {
        e.outcomes
            .iter()
            .filter(|o| matches!(o.role, capant::Role::Preferred(_)))
            .map(|o| o.state != capant::State::Yes)
            .collect()
    };
    ev.reorder_satisfiers(|a, b| {
        let (pa, pb) = (&qualified[position(a)], &qualified[position(b)]);
        met(a)
            .cmp(&met(b))
            .then_with(|| match (p50(pa), p50(pb)) {
                (Some(x), Some(y)) => x.cmp(&y),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => std::cmp::Ordering::Equal,
            })
            .then_with(|| pa.name.cmp(&pb.name))
    });
    ev.entries()
        .iter()
        .map(|e| {
            let provider = qualified[position(e)].clone();
            Sorted {
                p50_ns: p50(&provider),
                preferred: needs
                    .preferred
                    .iter()
                    .enumerate()
                    .map(|(i, id)| {
                        let yes = e.outcomes.iter().any(|o| {
                            o.role == capant::Role::Preferred(i) && o.state == capant::State::Yes
                        });
                        (id.clone(), yes)
                    })
                    .collect(),
                provider,
            }
        })
        .collect()
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
        format!("capability\t{id}\t{backend}\t1\t\t{e}\t{r}\tprobe=t::{id}\t\n")
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
        assert!(text.contains("[a: kv.ttl unverified(no row)]"), "{text}");
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
            host: None,
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
            host: None,
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

    #[test]
    fn a_yes_row_without_its_probe_id_is_unverified_and_the_provider_is_rejected() {
        let table = [prov("a", &["a"], decl(Durability::Machine))];
        let n = needs(&["kv.get"], Durability::Volatile);
        // Control: the same row with a probe id qualifies.
        assert_eq!(
            providers(&table, &n, &matrix(&[("kv.get", "a", true)]))
                .unwrap()
                .len(),
            1
        );
        // Drop the evidence: `capant` does not count a claim without it.
        let bare = Matrix::parse(&format!(
            "{HEADER}capability	kv.get	a	1		yes			
"
        ))
        .unwrap();
        let e = providers(&table, &n, &bare).unwrap_err();
        assert_eq!(e.providers[0].missing, vec![(id("kv.get"), Lookup::NoRow)]);
        assert_eq!(
            e.providers[0].unverified,
            vec![(id("kv.get"), "no evidence".to_string())]
        );
        assert!(
            e.to_string()
                .contains("[a: kv.get unverified(no evidence)]"),
            "{e}"
        );
    }

    #[test]
    fn the_declared_class_is_what_a_durability_capability_rests_on() {
        let n = needs(&["kv.get"], Durability::Machine);
        let m = matrix(&[("kv.get", "a", true), ("kv.get", "b", true)]);
        let table = [
            prov("a", &["a"], decl(Durability::Process)),
            prov("b", &["b"], decl(Durability::Machine)),
        ];
        let got = providers(&table, &n, &m).unwrap();
        assert_eq!(got.iter().map(Provider::name).collect::<Vec<_>>(), ["b"]);
        // The shortfall names what the other declared, and a missing declaration is unknown.
        let e = providers(&table[..1], &n, &m).unwrap_err();
        assert_eq!(
            e.providers[0].durability,
            Some(DurabilityShortfall::Below(Durability::Process))
        );
        let none = [prov("c", &["a"], None)];
        let e = providers(&none, &n, &m).unwrap_err();
        assert_eq!(
            e.providers[0].durability,
            Some(DurabilityShortfall::Unknown)
        );
        // Control: Volatile asks for no durability capability, so an undeclared provider qualifies.
        let v = needs(&["kv.get"], Durability::Volatile);
        assert_eq!(providers(&none, &v, &m).unwrap().len(), 1);
    }

    #[test]
    fn providers_keep_table_order_sorted_follows_the_preferences_and_a_repeated_name_is_fine() {
        let m = matrix(&[
            ("kv.get", "a", true),
            ("kv.get", "b", true),
            ("kv.ttl", "b", true),
        ]);
        let table = [
            prov("a", &["a"], decl(Durability::Machine)),
            prov("b", &["b"], decl(Durability::Machine)),
        ];
        let n = needs(&["kv.get"], Durability::Volatile).prefer(vec![id("kv.ttl")]);
        let q = providers(&table, &n, &m).unwrap();
        assert_eq!(q.iter().map(Provider::name).collect::<Vec<_>>(), ["a", "b"]);
        let s = sorted(&q, &n, &m, None);
        assert_eq!(
            s.iter().map(|x| x.provider.name()).collect::<Vec<_>>(),
            ["b", "a"]
        );
        assert_eq!(s[0].preferred, vec![(id("kv.ttl"), true)]);
        assert_eq!(s[1].preferred, vec![(id("kv.ttl"), false)]);
        // Two providers under one name no longer trip `capant`: both are returned.
        let twice = [table[0].clone(), table[0].clone()];
        assert_eq!(providers(&twice, &n, &m).unwrap().len(), 2);
    }
}
