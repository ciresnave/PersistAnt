// SPDX-License-Identifier: MIT OR Apache-2.0
//! Stage 4 of backend benchmarking for the relational kind: **"best backend for X"**, as a ranked list
//! *with its evidence*. The logic is kind-agnostic and lives in [`persistant_core::choose`]; this
//! module supplies what is specific to relational databases: the table of what each backend declares
//! about its durability, the typed way to name a need, and the contract check that makes a candidate.
//!
//! **Durability is declared and, for the relational kind, not verified at all yet**: the kv kind has a
//! process-kill test (`kv.durability.process`) that exercises these databases through
//! `KvOnRelational`, but there is no relational-level one, so every `verified` here is `None`. A
//! power-loss test is not built.

use crate::RelationalBackend;
use crate::conformance::Leaf;
use crate::contract;
use crate::speed::{SpeedTable, Workload};
use persistant_core::Config;

pub use persistant_core::choose::{
    Candidate, Choice, Declared, Durability, Entry, Excluded, Need, NoneEligible, Unmeasured, Why,
};

/// What the relational databases document about their own durability, by the name they carry in the
/// matrix. `None` for a backend this table does not know: that is unknown, not volatile.
pub fn declared_durability(backend: &str) -> Option<Declared> {
    let d = |class, basis| {
        Some(Declared {
            class,
            basis,
            verified: None,
        })
    };
    match backend {
        "sqlite" => d(
            Durability::Process,
            "SQLite WAL file; sqlx's default synchronous=FULL is documented but not asserted or tested here",
        ),
        "postgres" => d(
            Durability::Machine,
            "PostgreSQL defaults (fsync=on, synchronous_commit=on); documented, no power-loss test",
        ),
        "mysql" => d(
            Durability::Machine,
            "InnoDB defaults (innodb_flush_log_at_trx_commit=1); documented, no power-loss test",
        ),
        _ => None,
    }
}

/// Name a need in relational terms: the leaves relied on, the lowest acceptable durability class, the
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
pub async fn candidate<R: RelationalBackend>(
    target: &R,
    config: &Config,
    leaves: &[Leaf],
) -> Candidate {
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

    #[test]
    fn no_relational_backend_is_marked_verified_and_an_unknown_one_is_unknown() {
        for b in ["sqlite", "postgres", "mysql"] {
            assert!(declared_durability(b).expect(b).verified.is_none(), "{b}");
        }
        assert!(declared_durability("some-other-db").is_none());
    }
}
