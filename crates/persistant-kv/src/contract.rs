// SPDX-License-Identifier: MIT OR Apache-2.0
//! Stage 1 of backend benchmarking: **eligibility**, pass or fail, nothing else.
//!
//! A consumer names the leaves it needs; a backend that does not pass every one of them is
//! [`Eligibility::Excluded`], with the reason per leaf. It is never "ranked slower": ranking exists
//! only among eligible backends, and only in later stages. A required leaf nobody tested is an
//! exclusion too (`Reason::Untested`), so a gap in the suite cannot read as a pass.

use crate::KvBackend;
use crate::conformance::{Certified, Leaf, certify_leaves};
use persistant_core::{Config, Reason};

/// One required leaf the backend did not pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exclusion {
    /// The leaf.
    pub leaf: Leaf,
    /// Why: `Missing` (not offered), `Wrong` (broke the contract) or `Untested`.
    pub reason: Reason,
    /// The test's own words.
    pub note: String,
}

/// Whether a backend may be considered at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Eligibility {
    /// Every required leaf passed, its controls held.
    Eligible,
    /// At least one did not. Never empty.
    Excluded(Vec<Exclusion>),
}

impl Eligibility {
    /// `true` for [`Eligibility::Eligible`].
    pub fn is_eligible(&self) -> bool {
        matches!(self, Eligibility::Eligible)
    }
}

/// Decide eligibility from certified rows. A required leaf with no row is `Untested`.
pub fn evaluate(rows: &[Certified], required: &[Leaf]) -> Eligibility {
    let mut out: Vec<Exclusion> = Vec::new();
    for leaf in required.iter().copied() {
        if out.iter().any(|e| e.leaf == leaf) {
            continue;
        }
        match rows.iter().find(|c| c.leaf == leaf) {
            Some(c) if c.row.enabled => {}
            Some(c) => out.push(Exclusion {
                leaf,
                reason: c.row.reason.unwrap_or(Reason::Untested),
                note: c.row.note.clone(),
            }),
            None => out.push(Exclusion {
                leaf,
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
pub async fn check<K: KvBackend>(target: &K, required: &[Leaf]) -> Eligibility {
    let rows = certify_leaves(target, &Config::new(), &Config::new(), required).await;
    evaluate(&rows, required)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Defect, KvOnBlob, MemoryKv};
    use persistant_blob::MemoryBlob;

    const BATCH: [Leaf; 3] = [
        Leaf::BatchRollback,
        Leaf::BatchConcurrentRead,
        Leaf::ScanValues,
    ];

    #[tokio::test]
    async fn a_correct_backend_is_eligible() {
        assert_eq!(check(&MemoryKv::new(), &BATCH).await, Eligibility::Eligible);
    }

    #[tokio::test]
    async fn a_backend_without_batch_is_excluded_as_missing_not_ranked() {
        let kv = KvOnBlob::new(MemoryBlob::new());
        match check(&kv, &BATCH).await {
            Eligibility::Excluded(ex) => {
                let rollback = ex
                    .iter()
                    .find(|e| e.leaf == Leaf::BatchRollback)
                    .expect("excluded");
                assert_eq!(rollback.reason, Reason::Missing);
            }
            Eligibility::Eligible => panic!("KvOnBlob offers no batch"),
        }
    }

    #[tokio::test]
    async fn a_defective_backend_is_excluded_as_wrong() {
        let leaf = Leaf::Delete;
        let d = Defect::DeleteIsIgnored;
        let r = check(&MemoryKv::with_defect(d), &[leaf]).await;
        assert!(
            matches!(&r, Eligibility::Excluded(ex) if ex[0].reason == Reason::Wrong),
            "{d:?}: {r:?}"
        );
        // Control: the same requirement passes on a correct backend.
        assert!(check(&MemoryKv::new(), &[leaf]).await.is_eligible());
    }

    #[test]
    fn a_required_leaf_with_no_row_is_untested_not_a_pass() {
        let r = evaluate(&[], &[Leaf::Get]);
        assert!(matches!(&r, Eligibility::Excluded(ex) if ex[0].reason == Reason::Untested));
    }

    #[test]
    fn an_empty_requirement_is_eligible() {
        assert!(evaluate(&[], &[]).is_eligible());
    }
}
