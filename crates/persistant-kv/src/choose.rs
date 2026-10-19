// SPDX-License-Identifier: MIT OR Apache-2.0
//! Stage 4 of backend benchmarking for the key-value kind: **"best backend for X"**, as a ranked list
//! *with its evidence*. The logic is kind-agnostic and lives in [`persistant_core::choose`]; this
//! module supplies what is specific to key-value stores: the table of what each backend declares about
//! its durability, the typed way to name a need, and the contract check that makes a candidate.
//!
//! See [`persistant_core::choose`] for the rules: nothing is chosen for the consumer, a backend that
//! fails a needed leaf or declares too little durability is excluded with the reason, nothing eligible
//! is an error with the reasons, and every decision can be logged ([`Choice::log_lines`]).
//!
//! **Durability is declared; `Process` is verified** by [`durability`](crate::durability)
//! (`kv.durability.process`), and `Machine` by the guest power-cut harness (`kv.durability.machine`,
//! `scripts/machine-durability`) for `kv-on-relational` on SQLite, PostgreSQL and MySQL only. Host power
//! loss, NTFS and remote object stores stay unverified.

use crate::KvBackend;
use crate::conformance::Leaf;
use crate::contract::{self, Eligibility as KvEligibility};
use crate::speed::{SpeedTable, Workload};
use persistant_core::Config;
use persistant_core::choose::{Eligibility, LeafFailure};

pub use persistant_core::choose::{
    Candidate, Choice, Declared, Durability, Entry, Excluded, Need, NoneEligible, Unmeasured, Why,
};

/// What the key-value backends document about their own durability, by the name they carry in the
/// matrix. `None` for a backend this table does not know: that is unknown, not volatile.
pub fn declared_durability(backend: &str) -> Option<Declared> {
    let d = |class, basis, verified| {
        Some(Declared {
            class,
            basis,
            verified,
        })
    };
    let p = Some(Durability::Process);
    // Run 39178436801 (main 02f86a7): a Hyper-V guest powered off after 300 acknowledged writes, five
    // cycles each, nothing lost; the synchronous=OFF control lost every key in all four cycles.
    let m = Some(Durability::Machine);
    match backend {
        "memory" | "kv-on-blob:opendal-memory" => {
            d(Durability::Volatile, "in-process memory", None)
        }
        "kv-on-blob:opendal-fs" => d(
            Durability::Process,
            "files written through the OS cache; no fsync is asserted",
            p,
        ),
        "kv-on-relational:sqlite" => d(
            Durability::Process,
            "SQLite WAL file opened with SqliteBackend::open; synchronous is not asserted, so the class is Process. Open with open_durable (kv-on-relational:sqlite-full) to declare Machine",
            p,
        ),
        "kv-on-relational:sqlite-full" => d(
            Durability::Machine,
            "SQLite WAL file, synchronous=FULL set explicitly (SqliteBackend::open_durable); verified: guest power loss on ext4 (Hyper-V), run 39178436801; UNVERIFIED: host power loss, NTFS, disk cache, torn sectors, PG/MySQL ENOSPC, remote stores",
            m,
        ),
        "kv-on-relational:postgres" => d(
            Durability::Machine,
            "PostgreSQL defaults (fsync=on, synchronous_commit=on); verified: guest power loss on ext4 (Hyper-V), run 39178436801; UNVERIFIED: host power loss, NTFS, disk cache, torn sectors, PG/MySQL ENOSPC, remote stores",
            m,
        ),
        "kv-on-relational:mysql" => d(
            Durability::Machine,
            "InnoDB defaults (innodb_flush_log_at_trx_commit=1); verified: guest power loss on ext4 (Hyper-V), run 39178436801; UNVERIFIED: host power loss, NTFS, disk cache, torn sectors, PG/MySQL ENOSPC, remote stores",
            m,
        ),
        _ => None,
    }
}

/// Name a need in key-value terms: the leaves relied on, the lowest acceptable durability class, the
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
pub async fn candidate<K: KvBackend>(target: &K, config: &Config, leaves: &[Leaf]) -> Candidate {
    let eligibility = match contract::check(target, leaves).await {
        KvEligibility::Eligible => Eligibility::Eligible,
        KvEligibility::Excluded(ex) => Eligibility::Excluded(
            ex.into_iter()
                .map(|e| LeafFailure {
                    leaf: e.leaf.id().to_string(),
                    reason: e.reason,
                    note: e.note,
                })
                .collect(),
        ),
    };
    Candidate {
        backend: target.backend().to_string(),
        version: target.version().to_string(),
        config: config.clone(),
        eligibility,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::speed::Environment;
    use crate::{Defect, KvOnBlob, MemoryKv};
    use persistant_blob::MemoryBlob;
    use persistant_core::Reason;

    #[test]
    fn machine_is_verified_only_where_the_power_cut_harness_ran() {
        let machine = [
            "kv-on-relational:sqlite-full",
            "kv-on-relational:postgres",
            "kv-on-relational:mysql",
        ];
        for b in machine {
            assert_eq!(
                declared_durability(b).expect(b).verified,
                Some(Durability::Machine),
                "{b}"
            );
        }
        // The control: every other backend the table knows stays at or below Process.
        for b in [
            "memory",
            "kv-on-blob:opendal-memory",
            "kv-on-blob:opendal-fs",
            // The default open does not assert synchronous=FULL, so it is not Machine.
            "kv-on-relational:sqlite",
        ] {
            let d = declared_durability(b).expect(b);
            assert!(
                d.verified.is_none_or(|v| v <= Durability::Process),
                "{b}: the harness did not exercise it"
            );
            assert!(d.class <= Durability::Process, "{b}: declared too high");
        }
    }

    #[tokio::test]
    async fn the_contract_check_feeds_the_choice_end_to_end() {
        let e = Environment::detect("test", None);
        let good = MemoryKv::new();
        let lossy = MemoryKv::with_defect(Defect::DeleteIsIgnored);
        let blob = KvOnBlob::new(MemoryBlob::new());
        let leaves = [Leaf::Get, Leaf::Delete];
        let cands = [
            candidate(&good, &Config::new(), &leaves).await,
            candidate(&lossy, &Config::new(), &leaves).await,
            candidate(&blob, &Config::new(), &leaves).await,
        ];
        let table = SpeedTable::from_rows(
            crate::speed::run(&good, &Config::new(), &e, &[Workload::Get], 5).await,
        );
        let n = need(
            &leaves,
            Durability::Volatile,
            Workload::Get,
            e.comparable_key(),
        );
        let c = choose(&cands, &n, &table).unwrap();
        // MemoryKv (correct) is ranked; the lossy one is excluded as Wrong; the blob store passes the
        // leaves but was never timed here, so it is listed as unmeasured, not hidden.
        assert_eq!(c.ranked.len(), 1);
        assert_eq!(c.excluded.len(), 1);
        assert!(matches!(&c.excluded[0].why, Why::Contract(f) if f[0].reason == Reason::Wrong));
        assert_eq!(c.unmeasured.len(), 1);
    }

    #[tokio::test]
    async fn a_durability_minimum_uses_this_kinds_table() {
        let e = Environment::detect("test", None);
        let mem = MemoryKv::new();
        let table = SpeedTable::from_rows(
            crate::speed::run(&mem, &Config::new(), &e, &[Workload::Get], 5).await,
        );
        let cands = [candidate(&mem, &Config::new(), &[Leaf::Get]).await];
        let n = need(
            &[Leaf::Get],
            Durability::Process,
            Workload::Get,
            e.comparable_key(),
        );
        let err = choose(&cands, &n, &table).unwrap_err();
        assert!(matches!(
            err.excluded[0].why,
            Why::Durability {
                declared: Durability::Volatile,
                wanted: Durability::Process
            }
        ));
    }
}
