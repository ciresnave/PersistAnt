// SPDX-License-Identifier: MIT OR Apache-2.0
//! The checked-in `matrix/sqlx-mysql.tsv` parses and records every certified leaf on.

use persistant_core::{Config, Lookup, Matrix};

#[test]
fn the_recorded_mysql_rows_are_all_on_and_read_by_the_gate() {
    let m = Matrix::parse(include_str!("../../../matrix/sqlx-mysql.tsv"))
        .expect("the checked-in matrix parses");
    assert_eq!(
        m.rows().len(),
        24,
        "seventeen kv/lock leaves and seven relational leaves are certified"
    );
    for r in m.rows() {
        assert!(r.enabled, "{}: {}", r.subject, r.note);
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
