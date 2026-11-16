// SPDX-License-Identifier: MIT OR Apache-2.0
//! The checked-in `matrix/sqlx-mysql.tsv` parses and records every certified leaf on.
#![cfg(feature = "core")]

use persistant::capability::{Config, Lookup, Matrix};

#[test]
fn the_recorded_mysql_rows_are_all_on_and_read_by_the_gate() {
    let m = Matrix::parse(include_str!("../matrix/sqlx-mysql.tsv"))
        .expect("the checked-in matrix parses");
    assert_eq!(
        m.rows().len(),
        45,
        "twenty-two kv/lock leaves (kv.bounded_call twice: unset and set) and nineteen relational leaves (thirteen contract, six migration) are certified, and kv.delete_erased once, off as missing, and text.index once, on, and text.index.erased once, off as missing"
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
        // No erasing statement on this server: recorded off, missing, from the call itself.
        if matches!(r.subject.as_str(), "kv.delete_erased" | "text.index.erased") {
            assert!(!r.enabled, "{}", r.note);
            assert_eq!(r.reason, Some(persistant::capability::Reason::Missing));
            assert!(r.backend.starts_with("kv-on-relational:"), "{}", r.backend);
            continue;
        }
        // MySQL commits DDL implicitly: that leaf is off.
        assert_eq!(
            r.enabled,
            !matches!(
                r.subject.as_str(),
                "rel.migrate.ddl_atomic" | "rel.advisory_lock"
            ),
            "{}: {}",
            r.subject,
            r.note
        );
        // kv rows are keyed by the adapter, relational rows by the server itself.
        let want = if r.subject.as_str().starts_with("rel.") {
            "mysql"
        } else {
            "kv-on-relational:mysql"
        };
        assert_eq!(r.backend, want);
        assert_eq!(r.config.get("dialect"), Some("mysql"));
        assert!(r.metadata.get("test").is_some());
    }
    let first = &m.rows()[0];
    let actual = Config::new().with("dialect", "mysql");
    assert_eq!(
        m.lookup(&first.subject, &first.backend, &first.version, &actual),
        Lookup::On
    );
    assert_eq!(
        m.lookup(&first.subject, &first.backend, "0.0-not-certified", &actual),
        Lookup::NoRow
    );
}
