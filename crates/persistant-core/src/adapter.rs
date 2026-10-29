// SPDX-License-Identifier: MIT OR Apache-2.0
//! The matrix file is read and written by `capant` (`capant::parse` / `capant::write`); this module
//! maps between its rows and this crate's [`Row`], which stays what the gate, the conformance
//! suites and the registry use.
//!
//! The mapping, row by row:
//! - **Probe id.** `capant` keeps it in the evidence and writes it as the `probe` metadata key; this
//!   crate's rows carry it as the `test` metadata key. Reading maps `probe` back to `test`; writing
//!   maps `test` to `probe`. (If a row names both keys, `probe` is the id and `test` stays an
//!   ordinary fact, as in `capant`; read back, the id is then named `probe`.)
//! - **State.** `yes` is on; `missing`, `wrong` and `untested` are the three [`Reason`]s. Any other
//!   `no` text, and `capant`'s `UNVERIFIED: ` spellings, are refused as [`ParseError::BadReason`],
//!   as before.
//! - **Optional columns** (`probed_at`, `tool_version`, `environment`, `inherited_from`) are
//!   evidence in `capant` and unknown columns here: they ride in [`Row::extra`] under their own
//!   names, in the file's spelling, so a rewrite loses nothing.

use crate::{CapabilityId, Config, ConfigError, IdError, ParseError, Reason, Row};
use std::collections::BTreeMap;

/// The optional `capant` columns that live in [`Row::extra`] here.
const EVIDENCE_COLUMNS: [&str; 4] = ["probed_at", "tool_version", "environment", "inherited_from"];

/// The probe id's metadata key in this crate's rows, and in the file.
const PROBE_HERE: &str = "test";
const PROBE_FILE: &str = "probe";

/// Read matrix text.
pub(crate) fn parse(text: &str) -> Result<Vec<Row>, ParseError> {
    let parsed = capant::parse(text).map_err(error_from)?;
    let (header, data) = lines_of(text);
    let reason_at = header.iter().position(|c| *c == "reason");
    parsed
        .matrix
        .rows()
        .iter()
        .zip(data)
        .map(|(r, (line, raw))| {
            let reason_text = reason_at
                .and_then(|i| raw.split('\t').nth(i))
                .unwrap_or_default();
            row_from(r, line, reason_text)
        })
        .collect()
}

/// The canonical text of `rows`.
pub(crate) fn write(rows: &[Row]) -> String {
    capant::write(&capant::Matrix::from_rows(
        rows.iter().map(row_to).collect(),
    ))
}

/// The header and the data lines `capant` turned into rows, with their 1-based line numbers (blank
/// lines and `#` comments are skipped, as `capant` skips them).
fn lines_of(text: &str) -> (Vec<&str>, Vec<(usize, &str)>) {
    let mut header = None;
    let mut data = Vec::new();
    for (i, raw) in text.lines().enumerate() {
        let l = raw.trim_end_matches('\r');
        if l.is_empty() || l.starts_with('#') {
            continue;
        }
        if header.is_none() {
            header = Some(l.split('\t').collect());
        } else {
            data.push((i + 1, l));
        }
    }
    (header.unwrap_or_default(), data)
}

fn error_from(e: capant::ParseError) -> ParseError {
    use capant::ParseError as E;
    match e {
        E::NoHeader => ParseError::NoHeader,
        E::MissingColumn(c) => ParseError::MissingColumn(c),
        E::DuplicateColumn(c) => ParseError::DuplicateColumn(c),
        E::FieldCount { line, want, got } => ParseError::FieldCount { line, want, got },
        E::BadId { line, error } => ParseError::BadId {
            line,
            error: match error {
                capant::IdError::Empty => IdError::Empty,
                capant::IdError::BadDot => IdError::BadDot,
                capant::IdError::BadChar { at } => IdError::BadChar { at },
                _ => IdError::BadChar { at: 0 },
            },
        },
        E::BadEnabled { line, value } => ParseError::BadEnabled { line, value },
        E::ReasonMismatch { line } => ParseError::ReasonMismatch { line },
        E::BadConfig {
            line,
            column,
            error,
        } => ParseError::BadConfig {
            line,
            column,
            error: match error {
                capant::ConfigError::BadPair(p) => ConfigError::BadPair(p),
                capant::ConfigError::DuplicateKey(k) => ConfigError::DuplicateKey(k),
                other => ConfigError::BadPair(other.to_string()),
            },
        },
        E::BadTimestamp { line, value } => ParseError::BadTimestamp { line, value },
        other => ParseError::Other(other.to_string()),
    }
}

fn row_from(r: &capant::Row, line: usize, reason_text: &str) -> Result<Row, ParseError> {
    let subject =
        CapabilityId::new(r.subject.as_str()).map_err(|error| ParseError::BadId { line, error })?;
    let (enabled, reason) = match &r.state {
        capant::State::Yes => (true, None),
        capant::State::No(capant::Reason::Missing) => (false, Some(Reason::Missing)),
        capant::State::No(capant::Reason::Wrong) => (false, Some(Reason::Wrong)),
        capant::State::No(capant::Reason::Other(t)) if t == "untested" => {
            (false, Some(Reason::Untested))
        }
        _ => {
            return Err(ParseError::BadReason {
                line,
                value: reason_text.to_owned(),
            });
        }
    };
    let bad = |column: &'static str, error: ConfigError| ParseError::BadConfig {
        line,
        column,
        error,
    };
    let config = Config::parse(&r.config.to_string()).map_err(|e| bad("config", e))?;
    let mut metadata = Config::parse(&r.metadata.to_string()).map_err(|e| bad("metadata", e))?;
    if let Some(probe) = &r.evidence.probe {
        // `capant` leaves `test` in the metadata only when the row named both keys.
        let key = if metadata.get(PROBE_HERE).is_some() {
            PROBE_FILE
        } else {
            PROBE_HERE
        };
        metadata = metadata.with(key, probe.as_str());
    }
    let mut extra: BTreeMap<String, String> = r.extra.clone();
    let e = &r.evidence;
    if let Some(t) = e.probed_at {
        extra.insert("probed_at".into(), capant::time::format(t));
    }
    if let Some(v) = &e.tool_version {
        extra.insert("tool_version".into(), v.clone());
    }
    if !e.environment.is_empty() {
        extra.insert("environment".into(), e.environment.to_string());
    }
    if let Some(o) = &e.inherited_from {
        let v = if o.is_empty() {
            "?".to_owned()
        } else {
            o.to_string()
        };
        extra.insert("inherited_from".into(), v);
    }
    Ok(Row {
        subject_kind: r.subject_kind.clone(),
        subject,
        backend: r.backend.clone(),
        version: r.version.clone(),
        config,
        enabled,
        reason,
        metadata,
        note: r.note.clone(),
        extra,
    })
}

/// A `capant` config with the same facts (and none of this crate's key checks: `capant` writes
/// what it is given, as this crate did).
pub(crate) fn config_to(c: &Config, skip: Option<&str>) -> capant::Config {
    c.iter()
        .filter(|(k, _)| Some(*k) != skip)
        .fold(capant::Config::new(), |acc, (k, v)| acc.with(k, v))
}

fn row_to(r: &Row) -> capant::Row {
    let state = match (r.enabled, r.reason) {
        (true, _) => capant::State::Yes,
        (false, Some(Reason::Missing)) => capant::State::No(capant::Reason::Missing),
        (false, Some(Reason::Wrong)) => capant::State::No(capant::Reason::Wrong),
        (false, Some(Reason::Untested)) => {
            capant::State::No(capant::Reason::Other("untested".to_owned()))
        }
        // A `no` with no reason: written with an empty reason, which a reader refuses, as before.
        (false, None) => capant::State::No(capant::Reason::Other(String::new())),
    };
    let probe_key = [PROBE_FILE, PROBE_HERE]
        .into_iter()
        .find(|k| r.metadata.get(k).is_some());
    let mut evidence = capant::Evidence {
        probe: probe_key.and_then(|k| r.metadata.get(k)).map(str::to_owned),
        ..capant::Evidence::default()
    };
    let mut extra = r.extra.clone();
    let mut take = |name: &str| extra.remove(name).filter(|v| !v.is_empty());
    for name in EVIDENCE_COLUMNS {
        let Some(v) = take(name) else { continue };
        // A value this crate's reader could not have produced is dropped, not guessed at.
        match name {
            "probed_at" => evidence.probed_at = capant::time::parse(&v),
            "tool_version" => evidence.tool_version = Some(v),
            "environment" => evidence.environment = capant::Config::parse(&v).unwrap_or_default(),
            _ => {
                evidence.inherited_from = if v == "?" {
                    Some(capant::Config::new())
                } else {
                    capant::Config::parse(&v).ok()
                }
            }
        }
    }
    capant::Row {
        subject_kind: r.subject_kind.clone(),
        subject: capant::CapabilityId::new(r.subject.as_str())
            .expect("this crate's id grammar is inside capant's"),
        backend: r.backend.clone(),
        version: r.version.clone(),
        config: config_to(&r.config, None),
        state,
        evidence,
        note: r.note.clone(),
        metadata: config_to(&r.metadata, probe_key),
        extra,
    }
}

/// The rows as a `capant` matrix, for asking `capant` what this crate's gate asks (PM [TASK]
/// 2026-10-24, capant step 2, PR-a: nothing here calls it yet but tests).
///
/// Every row is a `capability` row, whatever its `subject_kind`, because [`crate::Matrix::lookup`]
/// does not look at the kind either. A `yes` carries its probe id as evidence (the `test` metadata
/// key, as the file spells it `probe`); a `yes` row with no probe id is therefore `Unverified`
/// to `capant` (no evidence) where this crate's gate says on: the checked-in matrices have none.
pub(crate) fn view(rows: &[Row]) -> capant::Matrix {
    capant::Matrix::from_rows(
        rows.iter()
            .map(|r| {
                let mut c = row_to(r);
                c.subject_kind = "capability".to_owned();
                c
            })
            .collect(),
    )
}
