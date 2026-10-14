// SPDX-License-Identifier: MIT OR Apache-2.0
use crate::row::COLUMNS;
use crate::{CapabilityId, Config, ParseError, Reason, Row};
use std::collections::BTreeSet;

/// What the matrix says about one capability on one `(backend, version, config)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Lookup {
    /// A matching row says yes and no matching row says no.
    On,
    /// A matching row says no, for this reason.
    Off(Reason),
    /// No row matches: untested, therefore off.
    NoRow,
}

/// The recorded facts of one or more matrix files, and the lookup the gate performs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Matrix {
    rows: Vec<Row>,
}

impl Matrix {
    /// An empty matrix: everything is off.
    pub fn new() -> Self {
        Matrix::default()
    }

    /// Read one tab-separated file. The first non-comment line is the header; columns are matched
    /// by name, so order does not matter and unknown columns are kept in [`Row::extra`]. Blank
    /// lines and lines starting with `#` are skipped.
    pub fn parse(text: &str) -> Result<Matrix, ParseError> {
        let mut header: Option<Vec<&str>> = None;
        let mut rows = Vec::new();
        for (i, raw) in text.lines().enumerate() {
            let line = i + 1;
            let l = raw.trim_end_matches('\r');
            if l.is_empty() || l.starts_with('#') {
                continue;
            }
            let fields: Vec<&str> = l.split('\t').collect();
            match &header {
                None => {
                    for c in COLUMNS {
                        if !fields.contains(&c) {
                            return Err(ParseError::MissingColumn(c));
                        }
                    }
                    header = Some(fields);
                }
                Some(h) => {
                    if fields.len() != h.len() {
                        return Err(ParseError::FieldCount {
                            line,
                            want: h.len(),
                            got: fields.len(),
                        });
                    }
                    rows.push(Row::parse_fields(h, &fields, line)?);
                }
            }
        }
        if header.is_none() {
            return Err(ParseError::NoHeader);
        }
        Ok(Matrix { rows })
    }

    /// A matrix of exactly these rows (what a conformance run produces).
    pub fn from_rows(rows: Vec<Row>) -> Matrix {
        Matrix { rows }
    }

    /// Add the rows of another file. A reader merges files; nothing is deduplicated.
    pub fn merge(&mut self, other: Matrix) {
        self.rows.extend(other.rows);
    }

    /// The rows, in file order.
    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    /// Write the known columns, then the sorted union of unknown columns. Round-trips what
    /// [`Matrix::parse`] read, apart from comments, blank lines and column order.
    pub fn to_tsv(&self) -> String {
        let extra: BTreeSet<&str> = self
            .rows
            .iter()
            .flat_map(|r| r.extra.keys().map(String::as_str))
            .collect();
        let columns: Vec<&str> = COLUMNS.iter().copied().chain(extra).collect();
        let mut out = columns.join("\t");
        out.push('\n');
        for r in &self.rows {
            let cells: Vec<String> = columns.iter().map(|c| r.field(c)).collect();
            out.push_str(&cells.join("\t"));
            out.push('\n');
        }
        out
    }

    /// The gate's lookup. Rows match on `subject`, `backend` and `version` exactly, and on `config`
    /// when every fact the row names is also in `actual`. **Off wins**: one matching `no` makes the
    /// answer off even if another matching row says yes.
    pub fn lookup(
        &self,
        subject: &CapabilityId,
        backend: &str,
        version: &str,
        actual: &Config,
    ) -> Lookup {
        let mut on = false;
        let mut off = None;
        for r in &self.rows {
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

    /// Whether the capability is on. Anything but [`Lookup::On`] is false.
    pub fn enabled(
        &self,
        subject: &CapabilityId,
        backend: &str,
        version: &str,
        actual: &Config,
    ) -> bool {
        self.lookup(subject, backend, version, actual) == Lookup::On
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: &str = "\
# comment
subject_kind\tsubject\tbackend\tversion\tconfig\tenabled\treason\tmetadata\tnote\tfrom
construct\ttext_comparison\tmysql\t8.4.11\tcollation=utf8mb4_bin\tyes\t\ttest=a::b\t\tpostgres
construct\ttext_comparison\tmysql\t8.4.11\tcollation=utf8mb4_0900_ai_ci\tno\twrong\ttest=a::b\tdiverges\tpostgres
construct\tcast.to_uuid\tmysql\t8.4.11\t\tno\tmissing\ttest=c::d\t\tpostgres
";

    fn id(s: &str) -> CapabilityId {
        CapabilityId::new(s).unwrap()
    }

    #[test]
    fn reads_a_unisql_shaped_file_and_keeps_the_extra_column() {
        let m = Matrix::parse(FILE).unwrap();
        assert_eq!(m.rows().len(), 3);
        assert_eq!(
            m.rows()[0].extra.get("from").map(String::as_str),
            Some("postgres")
        );
        let again = Matrix::parse(&m.to_tsv()).unwrap();
        assert_eq!(again, m, "round trip");
    }

    #[test]
    fn the_gate_is_on_only_for_a_matching_yes() {
        let m = Matrix::parse(FILE).unwrap();
        let bin = Config::parse("collation=utf8mb4_bin").unwrap();
        let ci = Config::parse("collation=utf8mb4_0900_ai_ci").unwrap();
        let t = id("text_comparison");
        assert!(m.enabled(&t, "mysql", "8.4.11", &bin));
        assert_eq!(
            m.lookup(&t, "mysql", "8.4.11", &ci),
            Lookup::Off(Reason::Wrong),
            "known-bad row"
        );
        assert_eq!(
            m.lookup(&t, "mysql", "9.0.0", &bin),
            Lookup::NoRow,
            "unknown version is off"
        );
        assert_eq!(
            m.lookup(&t, "mysql", "8.4.11", &Config::new()),
            Lookup::NoRow,
            "config not readable is off"
        );
        assert_eq!(
            m.lookup(&id("cast.to_uuid"), "mysql", "8.4.11", &bin),
            Lookup::Off(Reason::Missing)
        );
        assert_eq!(
            m.lookup(&id("never.heard_of_it"), "mysql", "8.4.11", &bin),
            Lookup::NoRow
        );
    }

    #[test]
    fn off_wins_over_a_yes() {
        let text = "subject_kind\tsubject\tbackend\tversion\tconfig\tenabled\treason\tmetadata\tnote\n\
                    capability\tk\tb\t1\t\tyes\t\t\t\n\
                    capability\tk\tb\t1\tx=1\tno\twrong\t\t\n";
        let m = Matrix::parse(text).unwrap();
        let k = id("k");
        assert!(m.enabled(&k, "b", "1", &Config::new()));
        assert!(!m.enabled(&k, "b", "1", &Config::new().with("x", "1")));
    }

    #[test]
    fn malformed_files_are_errors_not_silence() {
        assert_eq!(Matrix::parse(""), Err(ParseError::NoHeader));
        assert!(matches!(
            Matrix::parse("subject\tbackend\n"),
            Err(ParseError::MissingColumn(_))
        ));
        let h =
            "subject_kind\tsubject\tbackend\tversion\tconfig\tenabled\treason\tmetadata\tnote\n";
        let bad_yes = format!("{h}c\tk\tb\t1\t\tmaybe\t\t\t\n");
        assert!(matches!(
            Matrix::parse(&bad_yes),
            Err(ParseError::BadEnabled { line: 2, .. })
        ));
        let no_reason = format!("{h}c\tk\tb\t1\t\tno\t\t\t\n");
        assert_eq!(
            Matrix::parse(&no_reason),
            Err(ParseError::ReasonMismatch { line: 2 })
        );
        let yes_reason = format!("{h}c\tk\tb\t1\t\tyes\twrong\t\t\n");
        assert_eq!(
            Matrix::parse(&yes_reason),
            Err(ParseError::ReasonMismatch { line: 2 })
        );
        let short = format!("{h}c\tk\tb\n");
        assert!(matches!(
            Matrix::parse(&short),
            Err(ParseError::FieldCount { line: 2, .. })
        ));
    }

    #[test]
    fn merge_concatenates() {
        let mut a = Matrix::parse(FILE).unwrap();
        a.merge(Matrix::parse(FILE).unwrap());
        assert_eq!(a.rows().len(), 6);
    }
}
