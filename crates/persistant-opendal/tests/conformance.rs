// SPDX-License-Identifier: MIT OR Apache-2.0
//! Runs the blob contract tests against real OpenDAL services and checks what they find.
//!
//! The assertions are findings about the service, not wishes: if OpenDAL changes, the test says
//! which leaf moved. The matrix a release records is the output of the same `certify` call.

use persistant_blob::conformance::{Leaf, certify};
use persistant_core::{Config, Reason};
use persistant_opendal::OpendalBlob;

fn summary(rows: &[persistant_blob::conformance::Certified]) -> String {
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

#[tokio::test]
async fn memory_service_gives_every_sequential_leaf() {
    let b = OpendalBlob::memory().unwrap();
    let rows = certify(&b, b.config(), &Config::new()).await;
    for c in &rows {
        assert!(c.row.enabled, "{}: {}", c.leaf.id(), summary(&rows));
    }
    assert_eq!(rows[0].row.backend, "opendal-memory");
}

#[tokio::test]
async fn fs_service_without_atomic_write_dir() {
    let dir = tempfile::tempdir().unwrap();
    let b = OpendalBlob::fs(dir.path(), None).unwrap();
    let rows = certify(&b, b.config(), &Config::new()).await;
    for c in &rows {
        assert!(c.row.enabled, "{}: {}", c.leaf.id(), summary(&rows));
    }
    assert_eq!(rows[0].row.config.get("atomic_write_dir"), Some("unset"));
}

#[tokio::test]
async fn fs_service_with_atomic_write_dir_is_recorded_under_that_fact() {
    let root = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let b = OpendalBlob::fs(root.path(), Some(scratch.path())).unwrap();
    let rows = certify(&b, b.config(), &Config::new()).await;
    for c in &rows {
        assert!(c.row.enabled, "{}: {}", c.leaf.id(), summary(&rows));
        assert_eq!(c.row.config.get("atomic_write_dir"), Some("set"));
    }
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
    let _ = Leaf::ALL;
}
