// SPDX-License-Identifier: MIT OR Apache-2.0
//! `Matrix::to_capant()` is a view of this crate's rows that `capant` can be asked about (capant step 2,
//! PR-a); selection goes through it since 0.12.0 (PR-b, `provider.rs`). This proves the view says what `Matrix::lookup` says: for
//! every row of the five checked-in matrices against every configuration they name, `capant::resolve`
//! gives the verdict the gate gives (`Yes` / `No(reason)` / `Unverified(no row)`), and
//! `capant::evaluate` says a provider satisfies a need exactly when the gate turns every capability on.
//!
//! The comparison is a function, and a control mutates one verdict in the view and requires the
//! comparison to notice.
#![cfg(feature = "registry")]

use capant::{Context, Need, State, Why};
use persistant::capability::{CapabilityId, Config, Lookup, Matrix, Reason, Row, capant_provider};
use std::collections::BTreeSet;

const FILES: [(&str, &str); 5] = [
    ("opendal", include_str!("../matrix/opendal.tsv")),
    ("sqlx-mariadb", include_str!("../matrix/sqlx-mariadb.tsv")),
    ("sqlx-mysql", include_str!("../matrix/sqlx-mysql.tsv")),
    ("sqlx-postgres", include_str!("../matrix/sqlx-postgres.tsv")),
    ("sqlx-sqlite", include_str!("../matrix/sqlx-sqlite.tsv")),
];

/// The scan `Matrix::lookup` made before it was answered by `capant::resolve` (0.13.7 and earlier),
/// kept here as the independent oracle: if `lookup` were `resolve`, comparing the two would prove
/// nothing. Rows match on subject, backend and version exactly and on config when it holds in
/// `actual`; a matching `no` beats a matching `yes`.
fn gate_scan(
    m: &Matrix,
    subject: &CapabilityId,
    backend: &str,
    version: &str,
    actual: &Config,
) -> Lookup {
    let mut on = false;
    let mut off = None;
    for r in m.rows() {
        if &r.subject == subject
            && r.backend == backend
            && r.version == version
            && r.config.holds_in(actual)
        {
            if r.enabled {
                on = true;
            } else if off.is_none() {
                off = r.reason;
            }
        }
    }
    match (on, off) {
        (_, Some(reason)) => Lookup::Off(reason),
        (true, None) => Lookup::On,
        (false, None) => Lookup::NoRow,
    }
}

fn ctx() -> Context {
    Context::new(0, capant::Config::new())
}

fn cid(id: &CapabilityId) -> capant::CapabilityId {
    capant::CapabilityId::new(id.as_str()).unwrap()
}

/// What the gate's answer is, in `capant`'s terms.
fn expected(l: Lookup) -> State {
    match l {
        Lookup::On => State::Yes,
        Lookup::Off(Reason::Missing) => State::No(capant::Reason::Missing),
        Lookup::Off(Reason::Wrong) => State::No(capant::Reason::Wrong),
        Lookup::Off(Reason::Untested) => State::No(capant::Reason::Other("untested".to_owned())),
        Lookup::NoRow => State::Unverified(Why::NoRow),
        _ => panic!("a Lookup this test does not know"),
    }
}

/// Every place the two disagree, for every row of `m` against every configuration `m` names (and
/// none). Empty means they agree.
fn disagreements(m: &Matrix, view: &capant::Matrix) -> (usize, Vec<String>) {
    let mut configs: Vec<Config> = vec![Config::new()];
    configs.extend(m.rows().iter().map(|r| r.config.clone()));
    let mut asked = 0;
    let mut bad = Vec::new();
    for r in m.rows() {
        for actual in &configs {
            let provider = capant_provider("p", &r.backend, &r.version, actual);
            let got = capant::resolve(view, &provider, &cid(&r.subject), &ctx(), false).state;
            let want = expected(gate_scan(m, &r.subject, &r.backend, &r.version, actual));
            asked += 1;
            if got != want {
                bad.push(format!(
                    "{} on {} {} [{}]: gate {want}, capant {got}",
                    r.subject, r.backend, r.version, actual
                ));
            }
        }
    }
    (asked, bad)
}

#[test]
fn capant_resolves_every_checked_in_row_as_the_gate_does() {
    let mut total = 0;
    for (name, text) in FILES {
        let m = Matrix::parse(text).unwrap();
        let (asked, bad) = disagreements(&m, &m.to_capant());
        total += asked;
        assert!(
            bad.is_empty(),
            "{name}: {} disagreements, first {:?}",
            bad.len(),
            bad.first()
        );
    }
    assert!(
        total > 1000,
        "control: the comparison asked {total} questions"
    );
}

#[test]
fn the_comparison_notices_a_mutated_verdict() {
    // The control: flip one verdict in the view and the same comparison must report it.
    let m = Matrix::parse(FILES[0].1).unwrap();
    let view = m.to_capant();
    let mut yes_to_no = view.rows().to_vec();
    let at = yes_to_no
        .iter()
        .position(|r| r.state == State::Yes)
        .unwrap();
    yes_to_no[at].state = State::No(capant::Reason::Missing);
    let (_, bad) = disagreements(&m, &capant::Matrix::from_rows(yes_to_no));
    assert!(!bad.is_empty(), "a yes turned into a no went unnoticed");

    let mut no_to_yes = view.rows().to_vec();
    let at = no_to_yes
        .iter()
        .position(|r| matches!(r.state, State::No(_)))
        .unwrap();
    no_to_yes[at].state = State::Yes;
    no_to_yes[at].evidence.probe = Some("x".into());
    let (_, bad) = disagreements(&m, &capant::Matrix::from_rows(no_to_yes));
    assert!(!bad.is_empty(), "a no turned into a yes went unnoticed");
}

#[test]
fn evaluate_satisfies_exactly_when_the_gate_turns_every_capability_on() {
    for (name, text) in FILES {
        let m = Matrix::parse(text).unwrap();
        let view = m.to_capant();
        // One provider per distinct (backend, version, config) the file names.
        let surfaces: BTreeSet<(String, String, String)> = m
            .rows()
            .iter()
            .map(|r| (r.backend.clone(), r.version.clone(), r.config.to_string()))
            .collect();
        let mut checked = 0;
        let mut satisfied = 0;
        for (backend, version, cfg) in &surfaces {
            let actual = Config::parse(cfg).unwrap();
            let subjects: Vec<CapabilityId> = m
                .rows()
                .iter()
                .filter(|r| &r.backend == backend && &r.version == version)
                .map(|r| r.subject.clone())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect();
            let table = [capant_provider("p", backend, version, &actual)];
            // Each capability alone, and the whole set together.
            let mut needs: Vec<Vec<CapabilityId>> =
                subjects.iter().map(|s| vec![s.clone()]).collect();
            needs.push(subjects.clone());
            for need in needs {
                let gate_on = need
                    .iter()
                    .all(|s| gate_scan(&m, s, backend, version, &actual) == Lookup::On);
                let n = Need::new(need.iter().map(cid).collect());
                let eval = capant::evaluate(&table, &n, &view, &ctx()).unwrap();
                assert_eq!(
                    eval.entries()[0].satisfies,
                    gate_on,
                    "{name}: {backend} {version} [{actual}] needs {need:?}"
                );
                checked += 1;
                satisfied += usize::from(gate_on);
            }
        }
        assert!(checked > 20, "{name}: control: {checked} needs checked");
        assert!(
            satisfied > 0 && satisfied < checked,
            "{name}: control: some needs are met and some are not ({satisfied}/{checked})"
        );
    }
}

#[test]
fn the_edges_of_the_mapping() {
    let h = "subject_kind\tsubject\tbackend\tversion\tconfig\tenabled\treason\tmetadata\tnote\n";
    // A `construct` row counts, as it does for the gate; `untested` is a no, as for the gate.
    let text = format!(
        "{h}construct\tk\tb\t1\t\tyes\t\tprobe=p::k\t\n\
         capability\tu\tb\t1\t\tno\tuntested\t\t\n\
         capability\tw\tb\t1\t\tyes\t\t\t\n"
    );
    let m = Matrix::parse(&text).unwrap();
    let (_, bad) = disagreements(&m, &m.to_capant());
    // The one place they differ, on purpose: `w` is on in the gate with no probe id on the row, and
    // `capant` will not call a `yes` without evidence `Yes`.
    let once: BTreeSet<&String> = bad.iter().collect();
    assert_eq!(once.len(), 1, "{bad:?}");
    assert!(bad[0].starts_with("w on b 1"), "{bad:?}");
    let w: &Row = m.rows().iter().find(|r| r.subject.as_str() == "w").unwrap();
    let got = capant::resolve(
        &m.to_capant(),
        &capant_provider("p", "b", "1", &Config::new()),
        &cid(&w.subject),
        &ctx(),
        false,
    );
    assert_eq!(got.state, State::Unverified(Why::NoEvidence));
    // `Matrix::lookup` is answered by `resolve` but keeps the gate's answer there: on.
    assert_eq!(
        m.lookup(&w.subject, "b", "1", &Config::new()),
        Lookup::On,
        "a yes with no probe id stays on for the gate"
    );
}

#[test]
fn lookup_is_the_old_scan_on_every_checked_in_row() {
    let mut asked = 0;
    for (name, text) in FILES {
        let m = Matrix::parse(text).unwrap();
        let mut configs: Vec<Config> = vec![Config::new()];
        configs.extend(m.rows().iter().map(|r| r.config.clone()));
        for r in m.rows() {
            for actual in &configs {
                asked += 1;
                assert_eq!(
                    m.lookup(&r.subject, &r.backend, &r.version, actual),
                    gate_scan(&m, &r.subject, &r.backend, &r.version, actual),
                    "{name}: {} on {} {} [{actual}]",
                    r.subject,
                    r.backend,
                    r.version
                );
            }
        }
    }
    assert!(asked > 1000, "control: {asked} questions");
}
