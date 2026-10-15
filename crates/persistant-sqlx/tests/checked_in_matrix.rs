// SPDX-License-Identifier: MIT OR Apache-2.0
//! The checked-in `matrix/sqlx-postgres.tsv` parses and records every certified leaf on.

use persistant_core::{Config, Lookup, Matrix};

#[test]
fn the_recorded_postgres_rows_are_all_on_and_read_by_the_gate() {
    let m = Matrix::parse(include_str!("../../../matrix/sqlx-postgres.tsv"))
        .expect("the checked-in matrix parses");
    assert_eq!(m.rows().len(), 15, "fifteen kv/lock leaves are certified");
    for r in m.rows() {
        assert!(r.enabled, "{}: {}", r.subject, r.note);
        assert_eq!(r.backend, "kv-on-relational:postgres");
        assert_eq!(r.config.get("dialect"), Some("postgres"));
        assert!(r.metadata.get("test").is_some());
    }
    // The lookup the gate performs, with the row's own key.
    let first = &m.rows()[0];
    let actual = Config::new().with("dialect", "postgres");
    assert_eq!(
        m.lookup(&first.subject, &first.backend, &first.version, &actual),
        Lookup::On
    );
    assert_eq!(
        m.lookup(&first.subject, &first.backend, "0.0-not-certified", &actual),
        Lookup::NoRow
    );
}
