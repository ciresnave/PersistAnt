// SPDX-License-Identifier: MIT OR Apache-2.0
//! Stage 1 of backend benchmarking for the blob kind: **eligibility**, pass or fail, nothing else.
//!
//! A consumer names the leaves it needs; a backend that does not pass every one of them is
//! `Eligibility::Excluded`, with the reason per leaf. It is never "ranked slower". A required leaf
//! nobody tested is an exclusion too (`Reason::Untested`), so a gap in the suite cannot read as a pass.
//! The result type is [`persistant_core::choose::Eligibility`], shared with every kind.

use crate::BlobBackend;
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

/// Run the required leaves (each against its positive and known-bad controls) on `target`.
pub async fn check<B: BlobBackend>(target: &B, required: &[Leaf]) -> Eligibility {
    let rows = certify_leaves(target, &Config::new(), &Config::new(), required).await;
    evaluate(&rows, required)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Defect, MemoryBlob};

    #[tokio::test]
    async fn a_correct_backend_is_eligible() {
        let leaves = [Leaf::Read, Leaf::Write, Leaf::CreateExclusive];
        assert_eq!(
            check(&MemoryBlob::new(), &leaves).await,
            Eligibility::Eligible
        );
    }

    #[tokio::test]
    async fn a_defective_backend_is_excluded_as_wrong_and_the_control_passes() {
        let r = check(
            &MemoryBlob::with_defect(Defect::DeleteIsIgnored),
            &[Leaf::Delete],
        )
        .await;
        assert!(
            matches!(&r, Eligibility::Excluded(ex) if ex[0].reason == Reason::Wrong && ex[0].leaf == "blob.delete"),
            "{r:?}"
        );
        assert_eq!(
            check(&MemoryBlob::new(), &[Leaf::Delete]).await,
            Eligibility::Eligible
        );
    }

    #[test]
    fn a_required_leaf_with_no_row_is_untested_not_a_pass() {
        let r = evaluate(&[], &[Leaf::Read]);
        assert!(matches!(&r, Eligibility::Excluded(ex) if ex[0].reason == Reason::Untested));
        assert_eq!(evaluate(&[], &[]), Eligibility::Eligible);
    }
}
