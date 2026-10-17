// SPDX-License-Identifier: MIT OR Apache-2.0
//! Stage 1 of backend benchmarking for the relational kind: **eligibility**, pass or fail, nothing else.
//!
//! A consumer names the leaves it needs; a backend that does not pass every one of them is
//! `Eligibility::Excluded`, with the reason per leaf. It is never "ranked slower". A required leaf
//! nobody tested is an exclusion too (`Reason::Untested`), so a gap in the suite cannot read as a pass.
//! The result type is [`persistant_core::choose::Eligibility`], shared with every kind.

use crate::RelationalBackend;
use crate::conformance::{Certified, Leaf, certify_leaves};
use persistant_core::choose::{Eligibility, LeafFailure};
use persistant_core::{Config, Reason};

/// Decide eligibility from certified rows. A required leaf with no row is `Untested`.
pub fn evaluate(rows: &[Certified], required: &[Leaf]) -> Eligibility {
    let mut out: Vec<LeafFailure> = Vec::new();
    for leaf in required.iter().copied() {
        if out.iter().any(|e| e.leaf == leaf.id()) {
            continue;
        }
        match rows.iter().find(|c| c.leaf == leaf) {
            Some(c) if c.row.enabled => {}
            Some(c) => out.push(LeafFailure {
                leaf: leaf.id().to_string(),
                reason: c.row.reason.unwrap_or(Reason::Untested),
                note: c.row.note.clone(),
            }),
            None => out.push(LeafFailure {
                leaf: leaf.id().to_string(),
                reason: Reason::Untested,
                note: "no test ran for this leaf".to_string(),
            }),
        }
    }
    if out.is_empty() {
        Eligibility::Eligible
    } else {
        Eligibility::Excluded(out)
    }
}

/// Run the required leaves (each against its known-bad controls) on `target`.
pub async fn check<R: RelationalBackend>(target: &R, required: &[Leaf]) -> Eligibility {
    let rows = certify_leaves(target, &Config::new(), &Config::new(), required).await;
    evaluate(&rows, required)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conformance::Certified;
    use persistant_core::Row;
    use std::collections::BTreeMap;

    fn row(leaf: Leaf, enabled: bool, reason: Option<Reason>) -> Certified {
        Certified {
            leaf,
            row: Row {
                subject_kind: "capability".to_string(),
                subject: persistant_core::CapabilityId::new(leaf.id()).unwrap(),
                backend: "test".to_string(),
                version: "0".to_string(),
                config: Config::new(),
                enabled,
                reason,
                metadata: Config::new(),
                note: "n".to_string(),
                extra: BTreeMap::new(),
            },
        }
    }

    #[test]
    fn a_leaf_that_is_on_is_eligible_and_one_that_is_off_is_excluded_with_its_reason() {
        let rows = [
            row(Leaf::Values, true, None),
            row(Leaf::Transaction, false, Some(Reason::Wrong)),
        ];
        assert_eq!(
            evaluate(&rows, &[Leaf::Values]),
            Eligibility::Eligible,
            "control: the passing leaf alone is eligible"
        );
        let r = evaluate(&rows, &[Leaf::Values, Leaf::Transaction]);
        assert!(
            matches!(&r, Eligibility::Excluded(ex) if ex.len() == 1 && ex[0].reason == Reason::Wrong && ex[0].leaf == "rel.transaction"),
            "{r:?}"
        );
    }

    #[test]
    fn a_required_leaf_with_no_row_is_untested_not_a_pass() {
        let r = evaluate(&[], &[Leaf::Values]);
        assert!(matches!(&r, Eligibility::Excluded(ex) if ex[0].reason == Reason::Untested));
        assert_eq!(evaluate(&[], &[]), Eligibility::Eligible);
    }
}
