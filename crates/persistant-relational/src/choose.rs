// SPDX-License-Identifier: MIT OR Apache-2.0
//! Stage 4 of backend benchmarking for the relational kind: **"best backend for X"**, as a ranked list
//! *with its evidence*. The logic is kind-agnostic and lives in [`persistant_core::choose`]; this
//! module supplies what is specific to relational databases: the table of what each backend declares
//! about its durability, the typed way to name a need, and the contract check that makes a candidate.
//!
//! **Durability is declared; `Machine` is verified only where the guest power-cut harness ran.** That
//! harness (`scripts/machine-durability`, run 39178436801) drives *key-value* writes through
//! `KvOnRelational` on SQLite (`open_durable`), PostgreSQL and MySQL, so what it shows is that an
//! acknowledged single-row write survives the loss of the guest OS (ext4, Hyper-V). It does not
//! exercise relational-level multi-statement transactions or DDL. There is no relational-level
//! process-kill test, so no backend here is verified as high as `Process` by this kind's own tests.

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
    let d = |class, basis, verified| {
        Some(Declared {
            class,
            basis,
            verified,
        })
    };
    // Run 39178436801 (main 02f86a7): a Hyper-V guest powered off after 300 acknowledged key-value
    // writes, five cycles each, nothing lost; the synchronous=OFF control lost every key.
    let m = Some(Durability::Machine);
    match backend {
        "sqlite" => d(
            Durability::Process,
            "SQLite WAL file opened with SqliteBackend::open; synchronous is not asserted, so the class is Process and nothing is verified. Open with open_durable (reports sqlite-full) to declare Machine",
            None,
        ),
        "sqlite-full" => d(
            Durability::Machine,
            "SQLite WAL file, synchronous=FULL set explicitly (SqliteBackend::open_durable); verified for acknowledged key-value writes: guest power loss on ext4 (Hyper-V), run 39178436801; not exercised: multi-statement transactions, DDL, host power loss, NTFS, disk cache, torn sectors",
            m,
        ),
        "postgres" => d(
            Durability::Machine,
            "PostgreSQL defaults (fsync=on, synchronous_commit=on); verified for acknowledged key-value writes: guest power loss on ext4 (Hyper-V), run 39178436801; not exercised: multi-statement transactions, DDL, host power loss, NTFS, disk cache, torn sectors",
            m,
        ),
        "mysql" => d(
            Durability::Machine,
            "InnoDB defaults (innodb_flush_log_at_trx_commit=1); verified for acknowledged key-value writes: guest power loss on ext4 (Hyper-V), run 39178436801; disk full STALLS, it does NOT fail loud: the client gets no answer and no error (run 39325665814), so a caller needs a call bound (kv.bounded_call / call_bound); not exercised: multi-statement transactions, DDL, host power loss, NTFS, disk cache, torn sectors",
            m,
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
    fn machine_is_verified_only_where_the_power_cut_harness_ran() {
        for b in ["sqlite-full", "postgres", "mysql"] {
            let d = declared_durability(b).expect(b);
            assert_eq!(d.class, Durability::Machine, "{b}");
            assert_eq!(d.verified, Some(Durability::Machine), "{b}");
        }
        // The default SQLite open asserts no synchronous mode: Process, nothing verified.
        let plain = declared_durability("sqlite").expect("sqlite");
        assert_eq!(plain.class, Durability::Process);
        assert!(plain.verified.is_none());
        assert!(declared_durability("some-other-db").is_none());
    }
}
