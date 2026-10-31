// SPDX-License-Identifier: MIT OR Apache-2.0
use crate::{CapabilityId, Config, ParseError, Reason, Row};
use std::sync::OnceLock;

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

/// A `capant` provider called `id` for the one place [`Matrix::lookup`] is asked about: this
/// `backend` at this `version` with these `actual` facts. Rows match it exactly when `lookup` would
/// match them.
pub fn capant_provider(
    id: &str,
    backend: &str,
    version: &str,
    actual: &Config,
) -> capant::Provider {
    let config = actual
        .iter()
        .fold(capant::Config::new(), |acc, (k, v)| acc.with(k, v));
    capant::Provider::new(id, vec![capant::Surface::new(backend, version, config)])
        .expect("a provider with one surface has a surface")
}

/// The recorded facts of one or more matrix files, and the lookup the gate performs.
#[derive(Debug, Clone, Default)]
pub struct Matrix {
    rows: Vec<Row>,
    /// `rows` as `capant` reads them, built on the first [`Matrix::lookup`] and dropped whenever
    /// `rows` changes. Not part of equality.
    view: OnceLock<capant::Matrix>,
}

impl PartialEq for Matrix {
    fn eq(&self, other: &Matrix) -> bool {
        self.rows == other.rows
    }
}

impl Eq for Matrix {}

impl Matrix {
    /// An empty matrix: everything is off.
    pub fn new() -> Self {
        Matrix::default()
    }

    /// Read one tab-separated file (by `capant`'s reader; see `adapter.rs`). The first non-comment
    /// line is the header; columns are matched by name, so order does not matter and unknown
    /// columns are kept in [`Row::extra`]. Blank lines and lines starting with `#` are skipped.
    /// The probe id is the `probe` metadata key of the file, and the `test` key of a [`Row`]
    /// (the older spelling of the file is read too).
    pub fn parse(text: &str) -> Result<Matrix, ParseError> {
        crate::adapter::parse(text).map(Matrix::from_rows)
    }

    /// A matrix of exactly these rows (what a conformance run produces).
    pub fn from_rows(rows: Vec<Row>) -> Matrix {
        Matrix {
            rows,
            view: OnceLock::new(),
        }
    }

    /// Add the rows of another file. A reader merges files; nothing is deduplicated.
    pub fn merge(&mut self, other: Matrix) {
        self.rows.extend(other.rows);
        self.view = OnceLock::new();
    }

    /// The rows, in file order.
    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    /// Write the known columns, then the sorted union of unknown columns (by `capant`'s writer;
    /// the probe id is written as the `probe` metadata key). Round-trips what [`Matrix::parse`]
    /// read, apart from comments, blank lines and column order. A value in `probed_at`,
    /// `environment` or `inherited_from` that the reader could not have produced is not written.
    pub fn to_tsv(&self) -> String {
        crate::adapter::write(&self.rows)
    }

    /// The rows as a `capant` matrix, so that `capant` can be asked what [`Matrix::lookup`] answers.
    /// Every row becomes a `capability` row (the lookup ignores `subject_kind` too). `Reason::Untested`
    /// becomes `State::No(Other("untested"))`, which keeps "a `no` row wins" exact; a `yes` row with
    /// no probe id (the `test` metadata key) is `Unverified(NoEvidence)` to `capant`.
    pub fn to_capant(&self) -> capant::Matrix {
        crate::adapter::view(&self.rows)
    }

    /// The gate's lookup, answered by `capant::resolve` over [`Matrix::to_capant`]. Rows match on
    /// `subject`, `backend` and `version` exactly, and on `config` when every fact the row names is
    /// also in `actual`. **Off wins**: one matching `no` makes the answer off even if another
    /// matching row says yes. A matching `yes` is [`Lookup::On`] even where `capant` would not call
    /// it `Yes` (no probe id on the row, evidence from another environment or past its age): the
    /// gate has always read the row, not its evidence, and this keeps that. Selection through
    /// `capant::evaluate` is stricter; whether the gate should be too is the owner's decision.
    pub fn lookup(
        &self,
        subject: &CapabilityId,
        backend: &str,
        version: &str,
        actual: &Config,
    ) -> Lookup {
        let view = self.view.get_or_init(|| self.to_capant());
        let provider = capant_provider("lookup", backend, version, actual);
        let id = capant::CapabilityId::new(subject.as_str())
            .expect("this crate's id grammar is inside capant's");
        let ctx = capant::Context::new(0, capant::Config::new());
        match capant::resolve(view, &provider, &id, &ctx, false).state {
            capant::State::Yes => Lookup::On,
            capant::State::No(capant::Reason::Missing) => Lookup::Off(Reason::Missing),
            capant::State::No(capant::Reason::Wrong) => Lookup::Off(Reason::Wrong),
            // `untested`, and a `no` with no reason (which a reader refuses): off, for want of a test.
            capant::State::No(_) => Lookup::Off(Reason::Untested),
            // A `yes` row `capant` distrusts: still on for the gate (see above).
            capant::State::Unverified(
                capant::Why::NoEvidence | capant::Why::OtherEnvironment | capant::Why::Stale,
            ) => Lookup::On,
            // No row, and any state `capant` adds later: not known to be on.
            _ => Lookup::NoRow,
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
