// SPDX-License-Identifier: MIT OR Apache-2.0
use crate::{CapabilityId, Config, ConfigError, IdError};
use std::collections::BTreeMap;
use std::fmt;

/// Why a capability is off.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum Reason {
    /// The backend does not give it (or the test found nothing there).
    Missing,
    /// The backend gives something else: the result broke the contract.
    Wrong,
    /// No test ran for this `(backend, version, config)`.
    Untested,
}

impl Reason {
    /// The word used in the matrix file.
    pub fn as_str(self) -> &'static str {
        match self {
            Reason::Missing => "missing",
            Reason::Wrong => "wrong",
            Reason::Untested => "untested",
        }
    }
}

impl fmt::Display for Reason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Why matrix text could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ParseError {
    /// No header line.
    NoHeader,
    /// A required column is not in the header.
    MissingColumn(&'static str),
    /// A line has a different number of tab-separated fields than the header.
    FieldCount {
        line: usize,
        want: usize,
        got: usize,
    },
    /// `subject` is not a valid id.
    BadId { line: usize, error: IdError },
    /// `config` or `metadata` is not `k=v;k=v`.
    BadConfig {
        line: usize,
        column: &'static str,
        error: ConfigError,
    },
    /// `enabled` is not `yes` or `no`.
    BadEnabled { line: usize, value: String },
    /// `reason` is not `missing`, `wrong` or `untested`.
    BadReason { line: usize, value: String },
    /// `enabled=no` without a reason, or `enabled=yes` with one.
    ReasonMismatch { line: usize },
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseError::NoHeader => write!(f, "matrix has no header line"),
            ParseError::MissingColumn(c) => write!(f, "matrix header lacks column {c:?}"),
            ParseError::FieldCount { line, want, got } => {
                write!(f, "line {line}: {got} fields, header has {want}")
            }
            ParseError::BadId { line, error } => write!(f, "line {line}: subject: {error}"),
            ParseError::BadConfig {
                line,
                column,
                error,
            } => write!(f, "line {line}: {column}: {error}"),
            ParseError::BadEnabled { line, value } => {
                write!(f, "line {line}: enabled is {value:?}, want yes or no")
            }
            ParseError::BadReason { line, value } => {
                write!(f, "line {line}: reason {value:?} is unknown")
            }
            ParseError::ReasonMismatch { line } => {
                write!(
                    f,
                    "line {line}: a no needs a reason and a yes must have none"
                )
            }
        }
    }
}

impl std::error::Error for ParseError {}

/// The known columns, in the order they are written (`docs/DESIGN-KINDS.md` §2.4).
pub(crate) const COLUMNS: [&str; 9] = [
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

/// One recorded fact: a test ran, for this subject, on this backend, version and config.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// `capability`, `construct` or another word the writer chose; not interpreted here.
    pub subject_kind: String,
    /// The leaf id.
    pub subject: CapabilityId,
    /// The driver or dialect crate's backend name.
    pub backend: String,
    /// The backend (server) version string. No row for a version means off.
    pub version: String,
    /// Facts the gate can read at open, under which this row holds.
    pub config: Config,
    /// Boolean outcome.
    pub enabled: bool,
    /// Present exactly when `enabled` is false.
    pub reason: Option<Reason>,
    /// Environment, trial parameters, test id, tier, suite (`k=v;k=v`).
    pub metadata: Config,
    /// Free text; may carry `allowance=<name>` first.
    pub note: String,
    /// Columns this crate does not know (UniSQL writes `from`), kept so a rewrite loses nothing.
    pub extra: BTreeMap<String, String>,
}

impl Row {
    pub(crate) fn parse_fields(
        header: &[&str],
        fields: &[&str],
        line: usize,
    ) -> Result<Row, ParseError> {
        let idx = |name: &'static str| -> Result<usize, ParseError> {
            header
                .iter()
                .position(|h| *h == name)
                .ok_or(ParseError::MissingColumn(name))
        };
        let get = |name: &'static str| -> Result<&str, ParseError> { Ok(fields[idx(name)?]) };
        let cfg = |name: &'static str| -> Result<Config, ParseError> {
            Config::parse(get(name)?).map_err(|error| ParseError::BadConfig {
                line,
                column: name,
                error,
            })
        };
        let enabled = match get("enabled")? {
            "yes" => true,
            "no" => false,
            other => {
                return Err(ParseError::BadEnabled {
                    line,
                    value: other.to_string(),
                });
            }
        };
        let reason = match get("reason")? {
            "" => None,
            "missing" => Some(Reason::Missing),
            "wrong" => Some(Reason::Wrong),
            "untested" => Some(Reason::Untested),
            other => {
                return Err(ParseError::BadReason {
                    line,
                    value: other.to_string(),
                });
            }
        };
        if enabled == reason.is_some() {
            return Err(ParseError::ReasonMismatch { line });
        }
        let extra = header
            .iter()
            .zip(fields)
            .filter(|(h, _)| !COLUMNS.contains(h))
            .map(|(h, v)| (h.to_string(), v.to_string()))
            .collect();
        Ok(Row {
            subject_kind: get("subject_kind")?.to_string(),
            subject: CapabilityId::new(get("subject")?)
                .map_err(|error| ParseError::BadId { line, error })?,
            backend: get("backend")?.to_string(),
            version: get("version")?.to_string(),
            config: cfg("config")?,
            enabled,
            reason,
            metadata: cfg("metadata")?,
            note: get("note")?.to_string(),
            extra,
        })
    }

    pub(crate) fn field(&self, column: &str) -> String {
        match column {
            "subject_kind" => self.subject_kind.clone(),
            "subject" => self.subject.to_string(),
            "backend" => self.backend.clone(),
            "version" => self.version.clone(),
            "config" => self.config.to_string(),
            "enabled" => if self.enabled { "yes" } else { "no" }.to_string(),
            "reason" => self
                .reason
                .map(|r| r.as_str().to_string())
                .unwrap_or_default(),
            "metadata" => self.metadata.to_string(),
            "note" => self.note.clone(),
            other => self.extra.get(other).cloned().unwrap_or_default(),
        }
    }
}
