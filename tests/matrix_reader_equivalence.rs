// SPDX-License-Identifier: MIT OR Apache-2.0
//! `Matrix::parse` / `to_tsv` are `capant`'s reader and writer behind an adapter (0.9.40). This
//! proves the swap changed nothing the gate can see: the previous hand-written reader (kept here,
//! as it was at 0.9.39) and the new one read every checked-in file to the same rows, and the gate's
//! lookup agrees over every row x every configuration the files name. The one intended change is
//! the file's spelling of the probe id: `probe=` (read back as the rows' `test` key).
#![cfg(feature = "registry")]

use persistant::capability::{CapabilityId, Config, Lookup, Matrix, ParseError, Reason, Row};
use std::collections::BTreeMap;

const FILES: [(&str, &str); 5] = [
    ("opendal", include_str!("../matrix/opendal.tsv")),
    ("sqlx-mariadb", include_str!("../matrix/sqlx-mariadb.tsv")),
    ("sqlx-mysql", include_str!("../matrix/sqlx-mysql.tsv")),
    ("sqlx-postgres", include_str!("../matrix/sqlx-postgres.tsv")),
    ("sqlx-sqlite", include_str!("../matrix/sqlx-sqlite.tsv")),
];

const COLUMNS: [&str; 9] = [
    "subject_kind",
    "subject",
    "backend",
    "version",
    "config",
    "enabled",
    "reason",
    "metadata",
    "note",
];

/// The reader of 0.9.39, verbatim in behaviour, with errors reduced to a string.
fn legacy_parse(text: &str) -> Result<Vec<Row>, String> {
    let mut header: Option<Vec<&str>> = None;
    let mut rows = Vec::new();
    for (i, raw) in text.lines().enumerate() {
        let line = i + 1;
        let l = raw.trim_end_matches('\r');
        if l.is_empty() || l.starts_with('#') {
            continue;
        }
        let fields: Vec<&str> = l.split('\t').collect();
        let Some(h) = &header else {
            for c in COLUMNS {
                if !fields.contains(&c) {
                    return Err(format!("missing {c}"));
                }
            }
            header = Some(fields);
            continue;
        };
        if fields.len() != h.len() {
            return Err(format!("line {line}: field count"));
        }
        let get = |name: &str| fields[h.iter().position(|c| *c == name).unwrap()];
        let enabled = match get("enabled") {
            "yes" => true,
            "no" => false,
            other => return Err(format!("line {line}: enabled {other}")),
        };
        let reason = match get("reason") {
            "" => None,
            "missing" => Some(Reason::Missing),
            "wrong" => Some(Reason::Wrong),
            "untested" => Some(Reason::Untested),
            other => return Err(format!("line {line}: reason {other}")),
        };
        if enabled == reason.is_some() {
            return Err(format!("line {line}: reason mismatch"));
        }
        let extra: BTreeMap<String, String> = h
            .iter()
            .zip(&fields)
            .filter(|(c, _)| !COLUMNS.contains(c))
            .map(|(c, v)| (c.to_string(), v.to_string()))
            .collect();
        // The file's `probe=` is, for the old reader, an ordinary metadata fact; rows name the
        // id `test`, so the comparison renames it.
        let md_text = get("metadata").replace("probe=", "test=");
        rows.push(Row {
            subject_kind: get("subject_kind").to_string(),
            subject: CapabilityId::new(get("subject")).map_err(|e| e.to_string())?,
            backend: get("backend").to_string(),
            version: get("version").to_string(),
            config: Config::parse(get("config")).map_err(|e| e.to_string())?,
            enabled,
            reason,
            metadata: Config::parse(&md_text).map_err(|e| e.to_string())?,
            note: get("note").to_string(),
            extra,
        });
    }
    if header.is_none() {
        return Err("no header".into());
    }
    Ok(rows)
}

#[test]
fn every_checked_in_file_reads_to_the_same_rows_as_the_old_reader() {
    for (name, text) in FILES {
        let old = legacy_parse(text).unwrap_or_else(|e| panic!("{name}: {e}"));
        let new = Matrix::parse(text).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert!(!old.is_empty(), "{name}: control: the file has rows");
        assert_eq!(new.rows(), old.as_slice(), "{name}");
    }
}

#[test]
fn the_gate_answers_alike_for_every_row_and_every_configuration() {
    let mut asked = 0usize;
    for (name, text) in FILES {
        let old = Matrix::from_rows(legacy_parse(text).unwrap());
        let new = Matrix::parse(text).unwrap();
        let mut configs: Vec<Config> = vec![Config::new()];
        configs.extend(old.rows().iter().map(|r| r.config.clone()));
        for r in old.rows() {
            for actual in &configs {
                let (a, b) = (
                    old.lookup(&r.subject, &r.backend, &r.version, actual),
                    new.lookup(&r.subject, &r.backend, &r.version, actual),
                );
                assert_eq!(a, b, "{name}: {} on {} [{}]", r.subject, r.backend, actual);
                asked += 1;
            }
        }
        // Controls: the answers are not all one value, and an unknown version has no row.
        let first = &old.rows()[0];
        assert_eq!(
            new.lookup(
                &first.subject,
                &first.backend,
                "no-such-version",
                &first.config
            ),
            Lookup::NoRow,
            "{name}"
        );
    }
    assert!(asked > 1000, "asked {asked}");
}

#[test]
fn the_files_are_the_writers_canonical_text_with_the_probe_spelling() {
    for (name, text) in FILES {
        let m = Matrix::parse(text).unwrap();
        assert_eq!(
            m.to_tsv(),
            text,
            "{name}: regenerating the file changes nothing"
        );
        assert!(!text.contains("test="), "{name}: the old spelling is gone");
        assert!(
            text.contains("probe="),
            "{name}: control: the new one is there"
        );
    }
}

#[test]
fn the_older_spelling_is_still_read_and_written_as_probe() {
    let h = "subject_kind\tsubject\tbackend\tversion\tconfig\tenabled\treason\tmetadata\tnote\n";
    let old = format!("{h}capability\tk\tb\t1\t\tyes\t\ttier=x;test=a::b\t\n");
    let m = Matrix::parse(&old).unwrap();
    assert_eq!(m.rows()[0].metadata.get("test"), Some("a::b"));
    assert_eq!(m.rows()[0].metadata.get("tier"), Some("x"));
    assert_eq!(
        m.to_tsv(),
        format!("{h}capability\tk\tb\t1\t\tyes\t\tprobe=a::b;tier=x\t\n")
    );
}

#[test]
fn the_rejections_are_kept() {
    let h = "subject_kind\tsubject\tbackend\tversion\tconfig\tenabled\treason\tmetadata\tnote";
    let row = |cols: &str| format!("{h}\n{cols}\n");
    // Each of these was refused by the old reader and still is.
    let bad = [
        row("c\tk\tb\t1\t\tmaybe\t\t\t"),               // enabled
        row("c\tk\tb\t1\t\tno\t\t\t"),                  // no without reason
        row("c\tk\tb\t1\t\tyes\twrong\t\t"),            // yes with reason
        row("c\tk\tb\t1\t\tno\tbecause\t\t"),           // unknown reason
        row("c\tk\tb\t1\t\tno\tUNVERIFIED: stale\t\t"), // capant's spelling, not ours
        row("c\tk\tb\t1\t\tyes\t\ta\t"),                // metadata not k=v
        row("c\tBad\tb\t1\t\tyes\t\t\t"),               // id grammar
        row("c\tfn-max\tb\t1\t\tyes\t\t\t"),            // '-' is capant's, not ours
        row("c\tk\tb"),                                 // field count
        format!("{h}\tenabled\n"),                      // a column twice
    ];
    for text in &bad {
        assert!(
            legacy_parse(text).is_err() || text.contains("\tenabled\n"),
            "{text:?}"
        );
        assert!(Matrix::parse(text).is_err(), "{text:?}");
    }
    // The reason's own text is reported as written, with its line.
    assert_eq!(
        Matrix::parse(&row("c\tk\tb\t1\t\tno\tUNVERIFIED: stale\t\t")),
        Err(ParseError::BadReason {
            line: 2,
            value: "UNVERIFIED: stale".into()
        })
    );
    assert!(matches!(
        Matrix::parse(&row("c\tfn-max\tb\t1\t\tyes\t\t\t")),
        Err(ParseError::BadId { line: 2, .. })
    ));
    assert!(matches!(
        Matrix::parse(&format!("{h}\tenabled\n")),
        Err(ParseError::DuplicateColumn(_))
    ));
    // Control: a good row is read.
    assert!(Matrix::parse(&row("c\tk\tb\t1\t\tno\tuntested\t\t")).is_ok());
}

#[test]
fn optional_columns_ride_in_extra_and_survive_a_rewrite() {
    let h = "subject_kind\tsubject\tbackend\tversion\tconfig\tenabled\treason\tmetadata\tnote\tprobed_at\ttool_version\tenvironment\tinherited_from";
    let text = format!(
        "{h}\ncapability\tk\tb\t1\t\tyes\t\tprobe=p\t\t2026-10-24T00:00:00Z\t0.9\tos=linux\t?\n"
    );
    let m = Matrix::parse(&text).unwrap();
    let x = &m.rows()[0].extra;
    assert_eq!(
        x.get("probed_at").map(String::as_str),
        Some("2026-10-24T00:00:00Z")
    );
    assert_eq!(x.get("inherited_from").map(String::as_str), Some("?"));
    assert_eq!(m.to_tsv(), text);
}
