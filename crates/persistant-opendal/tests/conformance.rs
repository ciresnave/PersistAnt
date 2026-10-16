// SPDX-License-Identifier: MIT OR Apache-2.0
//! Runs the blob contract tests against real OpenDAL services and checks what they find.
//!
//! The assertions are findings about the service, not wishes: if OpenDAL changes, the test says
//! which leaf moved. The matrix a release records is the output of the same `certify` call.

use persistant_blob::conformance::{Certified, certify};
use persistant_core::{Config, Reason};
use persistant_opendal::OpendalBlob;

fn summary(rows: &[Certified]) -> String {
    rows.iter()
        .map(|c| {
            format!(
                "{}={}{}",
                c.leaf.id(),
                if c.row.enabled { "on" } else { "off" },
                c.row.reason.map(|r| format!("({r})")).unwrap_or_default()
            )
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// `None` = on; `Some(reason)` = off for that reason.
fn state(rows: &[Certified], id: &str) -> Option<Reason> {
    let c = rows
        .iter()
        .find(|c| c.leaf.id() == id)
        .unwrap_or_else(|| panic!("no row for {id}: {}", summary(rows)));
    if c.row.enabled { None } else { c.row.reason }
}

/// Whether the directory's filesystem treats names that differ only in case as the same name
/// (Windows, default macOS). Probed, not assumed from the OS.
fn case_insensitive(dir: &std::path::Path) -> bool {
    std::fs::write(dir.join("CaseProbe"), b"x").unwrap();
    let same = dir.join("caseprobe").exists();
    std::fs::remove_file(dir.join("CaseProbe")).unwrap();
    same
}

/// `blob.read` and `kv.get` check that keys differing only in case are distinct keys, so a
/// case-insensitive filesystem finds them wrong; every other leaf is unaffected.
fn expected(id: &str, case_insensitive: bool) -> Option<Reason> {
    if case_insensitive && (id == "blob.read" || id == "kv.get") {
        Some(Reason::Wrong)
    } else {
        None
    }
}

const SEQUENTIAL: [&str; 5] = [
    "blob.read",
    "blob.write",
    "blob.delete",
    "blob.stat",
    "blob.list",
];

#[tokio::test]
async fn memory_service_gives_everything_but_append() {
    let b = OpendalBlob::memory().unwrap();
    let rows = certify(&b, b.config(), &Config::new()).await;
    for c in &rows {
        let append = c.leaf.id().starts_with("blob.append");
        // Finding: OpenDAL's memory service does not support append.
        assert_eq!(c.row.enabled, !append, "{}", summary(&rows));
        if append {
            assert_eq!(c.row.reason, Some(Reason::Missing), "{}", summary(&rows));
        }
    }
    assert_eq!(rows[0].row.backend, "opendal-memory");
}

#[tokio::test]
async fn fs_without_atomic_write_dir_creates_exclusively_but_replaces_in_place() {
    let dir = tempfile::tempdir().unwrap();
    let ci = case_insensitive(dir.path());
    let b = OpendalBlob::fs(dir.path(), None).unwrap();
    let rows = certify(&b, b.config(), &Config::new()).await;
    for id in SEQUENTIAL {
        assert_eq!(state(&rows, id), expected(id, ci), "{}", summary(&rows));
    }
    assert_eq!(
        state(&rows, "blob.create_exclusive"),
        None,
        "{}",
        summary(&rows)
    );
    assert_eq!(
        state(&rows, "blob.replace_atomic.concurrent_read"),
        Some(Reason::Missing),
        "{}",
        summary(&rows)
    );
    for id in ["blob.append", "blob.append.concurrent_whole"] {
        assert_eq!(state(&rows, id), expected(id, ci), "{}", summary(&rows));
    }
    assert_eq!(rows[0].row.config.get("atomic_write_dir"), Some("unset"));
    let ex = rows
        .iter()
        .find(|c| c.leaf.id() == "blob.create_exclusive")
        .unwrap();
    assert_eq!(
        ex.row.metadata.get("contenders"),
        Some("16"),
        "trial parameters are recorded"
    );
}

#[tokio::test]
async fn fs_with_atomic_write_dir_replaces_atomically_but_cannot_create_exclusively() {
    let root = tempfile::tempdir().unwrap();
    let ci = case_insensitive(root.path());
    let scratch = tempfile::tempdir().unwrap();
    let b = OpendalBlob::fs(root.path(), Some(scratch.path())).unwrap();
    let rows = certify(&b, b.config(), &Config::new()).await;
    for id in SEQUENTIAL {
        assert_eq!(state(&rows, id), expected(id, ci), "{}", summary(&rows));
    }
    assert_eq!(
        state(&rows, "blob.replace_atomic.concurrent_read"),
        None,
        "{}",
        summary(&rows)
    );
    assert_eq!(
        state(&rows, "blob.create_exclusive"),
        Some(Reason::Missing),
        "README 0.4.7: check-then-rename; {}",
        summary(&rows)
    );
    assert!(
        rows.iter()
            .all(|c| c.row.config.get("atomic_write_dir") == Some("set"))
    );
}

#[tokio::test]
async fn a_service_that_cannot_work_is_missing_not_wrong() {
    // Control: a root that is a file cannot hold keys; every leaf must be off, none "on".
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("a-file");
    std::fs::write(&file, b"x").unwrap();
    let b = OpendalBlob::fs(&file, None).unwrap();
    let rows = certify(&b, b.config(), &Config::new()).await;
    assert!(rows.iter().all(|c| !c.row.enabled), "{}", summary(&rows));
    assert!(
        rows.iter().any(|c| c.row.reason == Some(Reason::Missing)),
        "{}",
        summary(&rows)
    );
}

/// Stage 2 (see `persistant_blob::stats`): on the memory service the concurrency leaves it offers
/// are exact on every one of five runs, and the append leaves it does not offer fail every run as
/// errors of kind `Unsupported`, not as a contract break (`wrong`) and not as a flaky pass.
#[tokio::test]
async fn memory_service_concurrency_leaves_are_exact_and_append_is_unsupported_every_run() {
    use persistant_blob::conformance::Leaf;
    use persistant_blob::stats::measure;
    let b = OpendalBlob::memory().unwrap();
    let on = [Leaf::CreateExclusive, Leaf::ReplaceConcurrentRead];
    for s in measure(&b, &on, 5).await {
        assert!(
            s.is_exact(),
            "{}: {}/{} runs passed, {} wrong, {} errored; first failure: {:?}",
            s.leaf.id(),
            s.passed,
            s.runs,
            s.wrong,
            s.errored,
            s.first_failure
        );
    }
    let off = [Leaf::Append, Leaf::AppendConcurrentWhole];
    for s in measure(&b, &off, 5).await {
        assert_eq!((s.passed, s.wrong, s.errored), (0, 0, 5), "{}", s.leaf.id());
        assert!(s.ops["append"].unsupported >= 5, "{:?}", s.ops);
    }
}

/// Stage 3 (see `persistant_kv::speed`): on `KvOnBlob` over OpenDAL memory the workloads that need
/// batch are excluded (Missing) and listed, the others are timed. Nothing is asserted about how fast.
#[tokio::test]
async fn speed_times_what_passes_the_contract_and_lists_what_does_not() {
    use persistant_kv::KvOnBlob;
    use persistant_kv::speed::{Environment, Outcome, Workload, run};
    let kv = KvOnBlob::new(OpendalBlob::memory().unwrap());
    let env = Environment::detect("cargo test -p persistant-opendal", None);
    let rows = run(
        &kv,
        &Config::new(),
        &env,
        &[Workload::Put, Workload::Get, Workload::Batch],
        10,
    )
    .await;
    assert!(matches!(rows[0].outcome, Outcome::Measured(_)));
    assert!(matches!(rows[1].outcome, Outcome::Measured(_)));
    assert!(
        matches!(
            &rows[2].outcome,
            Outcome::Excluded {
                reason: Reason::Missing,
                ..
            }
        ),
        "{:?}",
        rows[2].outcome
    );
}
