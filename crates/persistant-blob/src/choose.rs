// SPDX-License-Identifier: MIT OR Apache-2.0
//! Stage 4 of backend benchmarking for the blob kind: **"best backend for X"**, as a ranked list
//! *with its evidence*. The logic is kind-agnostic and lives in [`persistant_core::choose`]; this
//! module supplies what is specific to blob stores: the table of what each backend declares about its
//! durability, the typed way to name a need, and the contract check that makes a candidate.
//!
//! **Durability is declared and, for the blob kind, not verified at all yet**: the kv kind has a
//! process-kill test (`kv.durability.process`) that exercises the same OpenDAL services through
//! `KvOnBlob`, but there is no blob-level one, so every `verified` here is `None`. A power-loss test
//! (`blob.durable`) is not built.

use crate::BlobBackend;
use crate::conformance::Leaf;
use crate::contract;
use crate::speed::{SpeedTable, Workload};
use persistant_core::Config;

pub use persistant_core::choose::{
    Candidate, Choice, Declared, Durability, Entry, Excluded, Need, NoneEligible, Unmeasured, Why,
};

/// What the blob backends document about their own durability, by the name they carry in the matrix.
/// `None` for a backend this table does not know: that is unknown, not volatile.
pub fn declared_durability(backend: &str) -> Option<Declared> {
    let d = |class, basis| {
        Some(Declared {
            class,
            basis,
            verified: None,
        })
    };
    match backend {
        "memory" | "opendal-memory" => d(Durability::Volatile, "in-process memory"),
        "opendal-fs" => d(
            Durability::Process,
            "files written through the OS cache; no fsync is asserted",
        ),
        _ => None,
    }
}

/// Name a need in blob terms: the leaves relied on, the lowest acceptable durability class, the
/// workload to rank by, and the environment the ranking is for
/// ([`Environment::comparable_key`](crate::speed::Environment::comparable_key)).
pub fn need(
    leaves: &[Leaf],
    min_durability: Durability,
    rank_by: Workload,
    environment: impl Into<String>,
) -> Need {
    Need {
        leaves: leaves.iter().map(|l| l.id().to_string()).collect(),
        min_durability,
        rank_by: rank_by.id().to_string(),
        environment: environment.into(),
    }
}

/// Order `candidates` for `need` using `speed` and this kind's durability table.
pub fn choose(
    candidates: &[Candidate],
    need: &Need,
    speed: &SpeedTable,
) -> Result<Choice, NoneEligible> {
    persistant_core::choose::choose(candidates, need, speed, &declared_durability)
}

/// Run the contract check for the needed leaves on `target` and package the result.
pub async fn candidate<B: BlobBackend>(target: &B, config: &Config, leaves: &[Leaf]) -> Candidate {
    Candidate {
        backend: target.backend().to_string(),
        version: target.version().to_string(),
        config: config.clone(),
        eligibility: contract::check(target, leaves).await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::speed::{Environment, run};
    use crate::{Defect, MemoryBlob};
    use persistant_core::Reason;

    #[test]
    fn no_blob_backend_is_marked_verified() {
        for b in ["memory", "opendal-memory", "opendal-fs"] {
            assert!(declared_durability(b).expect(b).verified.is_none(), "{b}");
        }
    }

    #[tokio::test]
    async fn the_contract_check_feeds_the_choice_end_to_end() {
        let e = Environment::detect("test", None);
        let good = MemoryBlob::new();
        let lossy = MemoryBlob::with_defect(Defect::WriteIsLost);
        let leaves = [Leaf::Write, Leaf::Read];
        let cands = [
            candidate(&good, &Config::new(), &leaves).await,
            candidate(&lossy, &Config::new(), &leaves).await,
        ];
        let table =
            SpeedTable::from_rows(run(&good, &Config::new(), &e, &[Workload::Read], 5).await);
        let n = need(
            &leaves,
            Durability::Volatile,
            Workload::Read,
            e.comparable_key(),
        );
        let c = choose(&cands, &n, &table).unwrap();
        assert_eq!(c.ranked.len(), 1);
        assert_eq!(c.excluded.len(), 1);
        assert!(matches!(&c.excluded[0].why, Why::Contract(f) if f[0].reason == Reason::Wrong));
        let log = c.log_lines().join("\n");
        assert!(log.contains("verified: no"), "{log}");
    }
}
