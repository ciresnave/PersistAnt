// SPDX-License-Identifier: MIT OR Apache-2.0
//! The checked-in `matrix/sqlx-sqlite.tsv` parses and records every certified leaf on, except rel.row_lock (SQLite has no row locks).
#![cfg(feature = "core")]

use persistant::capability::{Config, Lookup, Matrix};

#[test]
fn the_recorded_sqlite_rows_are_all_on_and_read_by_the_gate() {
    let m = Matrix::parse(include_str!("../matrix/sqlx-sqlite.tsv"))
        .expect("the checked-in matrix parses");
    assert_eq!(
        m.rows().len(),
        78,
        "twenty-one kv/lock leaves on each of sqlite and sqlite-full (plus kv.bounded_call set on each), and seventeen relational leaves (eleven contract, six migration) on each of them are certified"
    );
    for r in m.rows() {
        // kv.bounded_call is recorded per call bound: off with none configured (`call_bound=unset`),
        // on when the backend was certified through `Bounded` (`call_bound=set`).
        let bound_row = r.subject.as_str() == "kv.bounded_call";
        if bound_row {
            assert_eq!(
                r.enabled,
                r.config.get("call_bound") == Some("set"),
                "{}",
                r.note
            );
            continue;
        }
        // SQLite has no row locks and holds the write lock from `begin`: those leaves are recorded off.
        assert_eq!(
            r.enabled,
            !matches!(
                r.subject.as_str(),
                "rel.row_lock" | "rel.isolation.repeatable_read" | "rel.isolation.serializable"
            ),
            "{}: {}",
            r.subject,
            r.note
        );
        // kv rows are keyed by the adapter (plain and the durable `open_durable` profile), relational
        // rows by the server itself.
        if r.subject.as_str().starts_with("rel.") {
            assert!(
                ["sqlite", "sqlite-full"].contains(&r.backend.as_str()),
                "{}",
                r.backend
            );
        } else {
            assert!(
                ["kv-on-relational:sqlite", "kv-on-relational:sqlite-full"]
                    .contains(&r.backend.as_str()),
                "{}",
                r.backend
            );
        }
        assert_eq!(r.config.get("dialect"), Some("sqlite"));
        assert!(r.metadata.get("test").is_some());
    }
    let first = &m.rows()[0];
    let actual = Config::new().with("dialect", "sqlite");
    assert_eq!(
        m.lookup(&first.subject, &first.backend, &first.version, &actual),
        Lookup::On
    );
    assert_eq!(
        m.lookup(&first.subject, &first.backend, "0.0-not-certified", &actual),
        Lookup::NoRow
    );
}
