// SPDX-License-Identifier: MIT OR Apache-2.0
use crate::{CapabilityId, Config, Reason, Row};
use std::collections::BTreeMap;
use std::fmt;

/// The result of running one capability's test, before anything is recorded.
///
/// The four outcomes of `docs/DESIGN-KINDS.md` §2.1 #3.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Verdict {
    /// The result matches the capability's written contract.
    Exact,
    /// Correct but different (pi as `3.141592` where `3.14159` was expected). The strict leaf stays
    /// off: either widen the test, if it was stricter than the written contract, or mint a new
    /// leaf with its own contract and a generalisation over the leaves. Never by editing the
    /// strict leaf's meaning.
    EquallyCorrectButDifferent,
    /// The backend broke the contract: off, and a bug report to the backend's developer.
    Wrong,
    /// The backend does not give it.
    Missing,
}

/// Whether the two controls of a test ran as they must.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Controls {
    /// The test passed against something known to be right.
    pub positive_passed: bool,
    /// The test **failed** against something known to be wrong (the known-bad run).
    pub known_bad_failed: bool,
}

/// An `on` was requested without both controls: the test proves nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ControlsNotRun(pub Controls);

impl fmt::Display for ControlsNotRun {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "an exact result is not recorded as on without both controls (positive passed: {}, known-bad failed: {})",
            self.0.positive_passed, self.0.known_bad_failed
        )
    }
}

impl std::error::Error for ControlsNotRun {}

impl Verdict {
    /// Turn a verdict into the row to record.
    ///
    /// Only [`Verdict::Exact`] needs [`Controls`], because only `on` is a claim; an off row is the
    /// safe default and can always be recorded.
    pub fn into_row(
        self,
        subject: CapabilityId,
        backend: impl Into<String>,
        version: impl Into<String>,
        config: Config,
        metadata: Config,
        controls: Controls,
    ) -> Result<Row, ControlsNotRun> {
        let (enabled, reason, note) = match self {
            Verdict::Exact => {
                if !(controls.positive_passed && controls.known_bad_failed) {
                    return Err(ControlsNotRun(controls));
                }
                (true, None, "")
            }
            Verdict::EquallyCorrectButDifferent => (
                false,
                Some(Reason::Wrong),
                "equally correct but different: widen the test only if stricter than the contract, else mint a leaf",
            ),
            Verdict::Wrong => (false, Some(Reason::Wrong), ""),
            Verdict::Missing => (false, Some(Reason::Missing), ""),
        };
        Ok(Row {
            subject_kind: "capability".to_string(),
            subject,
            backend: backend.into(),
            version: version.into(),
            config,
            enabled,
            reason,
            metadata,
            note: note.to_string(),
            extra: BTreeMap::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(v: Verdict, c: Controls) -> Result<Row, ControlsNotRun> {
        v.into_row(
            CapabilityId::new("kv.put_if_absent").unwrap(),
            "sqlite",
            "3.46",
            Config::new(),
            Config::new().with("test", "kv::put_if_absent_race"),
            c,
        )
    }

    const BOTH: Controls = Controls {
        positive_passed: true,
        known_bad_failed: true,
    };

    #[test]
    fn exact_is_on_only_with_both_controls() {
        let r = row(Verdict::Exact, BOTH).unwrap();
        assert!(r.enabled && r.reason.is_none());
        let no_known_bad = Controls {
            known_bad_failed: false,
            ..BOTH
        };
        assert_eq!(
            row(Verdict::Exact, no_known_bad),
            Err(ControlsNotRun(no_known_bad))
        );
        let no_positive = Controls {
            positive_passed: false,
            ..BOTH
        };
        assert!(row(Verdict::Exact, no_positive).is_err());
    }

    #[test]
    fn everything_else_is_off_with_the_right_reason_and_needs_no_controls() {
        let none = Controls {
            positive_passed: false,
            known_bad_failed: false,
        };
        assert_eq!(
            row(Verdict::Wrong, none).unwrap().reason,
            Some(Reason::Wrong)
        );
        assert_eq!(
            row(Verdict::Missing, none).unwrap().reason,
            Some(Reason::Missing)
        );
        let e = row(Verdict::EquallyCorrectButDifferent, none).unwrap();
        assert!(!e.enabled && e.reason == Some(Reason::Wrong) && e.note.contains("mint a leaf"));
    }
}
